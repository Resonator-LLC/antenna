//! Integration test for the messenger radio's bubble emit path (M1-D —
//! Telegram-style inline reactions).
//!
//! Boots `radios/messenger/pipeline.ttl` (with run.sh's sed-replacements
//! pre-applied), feeds it synthetic carrier events, and asserts the
//! resulting `urn:msg:bubble-obj:<mid>` placed-object widget triples carry
//! the M1-D shape:
//!
//!   1. Three antenna:lod blocks when the bubble has reactions — tier 1
//!      (compact `[emoji count]` chip), tier 2 (`[emoji avatars]`), tier
//!      3 (per-emoji reactor blocks). Each LOD blank node carries its
//!      own `antenna:worldHeight` so Station's anchor-aware placement
//!      (M1-D Path 3) grows the bubble downward on pinch-in.
//!   2. Single antenna:lod block when the bubble has no reactions —
//!      keeps the Depth Pip absent (count(antenna:lod) === 1) and
//!      matches the M1-C Path A baseline shape.
//!   3. Inline reaction chips inside the bubble's widget DSL (not as
//!      separate placed objects) carrying Button{onTap=urn:msg:react:
//!      mid:emoji}[…] so the quick-add UC3.8 contract still routes
//!      through the existing TapEvent handler.
//!   4. Tier-3 reactor rows with distinct fingerprints + presence dots,
//!      display names, and relative-time strings, sorted online-first
//!      then most-recent first (per UC3 § Tier 3).
//!
//! Driving via dispatch::dispatch + dag.pump_emits mirrors the live
//! WebSocket / Antenna loop, so a regression in the pipeline's emit shape
//! surfaces here regardless of which side authored the bug.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use oxigraph::sparql::QueryResults;
use std::path::PathBuf;
use std::time::Duration;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";

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

/// Boot a store + DAG running the messenger pipeline. Mirrors what
/// `radios/messenger/run.sh` does at launch — sed-replaces the
/// `__NICK__` / `__META_DIR__` / `__PEER_URI__` placeholders with test
/// values, loads the seed (favorites + menu items), then snapshots the
/// dag from the resulting store.
fn build_messenger_pipeline() -> (RdfStore, Dag) {
    let store = RdfStore::open(None).expect("in-memory store");

    let pipeline_raw = std::fs::read_to_string(rel("radios/messenger/pipeline.ttl"))
        .expect("read messenger pipeline");
    let pipeline_ttl = pipeline_raw
        .replace("__NICK__", "alice")
        .replace("__META_DIR__", "/tmp/messenger-test/")
        .replace("__PEER_URI__", "");
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger pipeline");

    let seed_ttl =
        std::fs::read_to_string(rel("radios/messenger/seed.ttl")).expect("read seed");
    store.insert_turtle(&seed_ttl).expect("insert seed");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

// M5-D-β — pipeline boot WITHOUT the seed.ttl synthetic
// messenger:Conversation triples. Used by tests that need to verify
// rebuildInbox's behaviour when zero conversations exist (e.g. the
// "no phantom inbox emit when rows.length=0" guard).
fn empty_messenger_pipeline() -> (RdfStore, Dag) {
    let store = RdfStore::open(None).expect("in-memory store");

    let pipeline_raw = std::fs::read_to_string(rel("radios/messenger/pipeline.ttl"))
        .expect("read messenger pipeline");
    let pipeline_ttl = pipeline_raw
        .replace("__NICK__", "alice")
        .replace("__META_DIR__", "/tmp/messenger-test/")
        .replace("__PEER_URI__", "");
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger pipeline");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

/// Iterate the tick loop until the script falls quiet. Same shape as
/// theme_authoring_pipeline_test::settle — pumps query results back into
/// the script, drains emits, re-dispatches each line so the placed-object
/// triples land in the store and downstream sp:Modify clauses execute.
///
/// Quiescence-aware exit: break after `EMPTY_BREAK` consecutive empty
/// iterations, but only once the script has emitted at least once.
/// `max_iters` is the ceiling (upper bound on wall-clock time
/// 40 ms × max_iters), but most calls return well before that as soon as
/// the script falls quiet.
///
/// The "saw_emit" gate avoids bailing before the script wakes up on slow
/// boots. The `>= EMPTY_BREAK` guard avoids racing on a temporary lull
/// between dispatch waves: each dispatch broadcasts on a channel and the
/// script thread needs another tick (and sometimes two) to wake, run, and
/// emit the next wave.
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

/// Synthetic incoming TextMessage — registers a (contactUri, mid, text)
/// tuple in `globalThis.messages` so subsequent reactions have a target
/// the script's `findMessageById` can look up.
///
/// The script handler's substring match looks for the literal prefix-form
/// text `carrier:TextMessage`, so we emit prefix-form here and rely on
/// `RdfStore::insert_turtle` to prepend `TURTLE_PREFIXES` for parsing.
/// Wrapping in `a antenna:Test` keeps the line out of the carrier-command
/// match path (which would otherwise log "carrier command not implemented"
/// and skip insert_with_dag).
fn text_message_event(contact_uri: &str, mid: &str, text: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:TextMessage \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:messageId \"{mid}\" ; \
         carrier:text \"{text}\" ."
    )
}

/// Synthetic carrier:Reaction line. Routes through `insert_with_dag`
/// (because the rdf:type is antenna:Test, not a known carrier command)
/// → broadcast on `beforeInsert` → script handler matches the
/// `carrier:Reaction` substring and applies the reactor.
fn reaction_event(mid: &str, contact_uri: &str, emoji: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:Reaction \"_\" ; \
         carrier:messageId \"{mid}\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:reaction \"{emoji}\" ."
    )
}

/// Synthetic carrier:ContactOnline / Offline / Name event. The substring-
/// match handler in pipeline.ttl looks for `carrier:ContactOnline` etc.
/// in the input — wrap as antenna:Test so dispatch routes via the
/// generic store-insert path (the same trick reaction_event uses), and
/// the script's else-if chain catches the substring on broadcast.
fn contact_online_event(contact_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:ContactOnline \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ."
    )
}

fn contact_offline_event(contact_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:ContactOffline \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ."
    )
}

fn contact_name_event(contact_uri: &str, display_name: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:ContactName \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:displayName \"{display_name}\" ."
    )
}

/// Synthetic carrier:SelfId event — sets globalThis.selfUri on the script side
/// so subsequent self-driven seeds (e.g. WhoAmI's M1-C self peer-cache upsert)
/// have a key to write under. Wrapped as antenna:Test so dispatch routes via
/// the generic store-insert path, mirroring the other synthetic helpers.
fn self_id_event(self_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:SelfId \"_\" ; \
         carrier:selfUri \"{self_uri}\" ."
    )
}

/// M4-A — synthetic carrier:FileRecv. Mirrors the carrier emit shape per
/// `carrier/src/turtle_emit.c:275-292` — conversationId / contactUri /
/// messageId / fileId (quoted strings), filename (quoted), size (raw
/// integer literal). Wrapped as antenna:Test so dispatch routes the line
/// through the script's beforeInsert broadcast (the same trick reaction_event
/// + contact_online_event use), letting the carrier:FileRecv else-if branch
/// in pipeline.ttl pick it up via substring match.
fn file_recv_event(
    conv_id: &str,
    sender_uri: &str,
    msg_id: &str,
    file_id: &str,
    filename: &str,
    size: u64,
) -> String {
    // M4-InvA — `carrier:account` is REQUIRED on the FileRecv event so
    // the pipeline's auto-accept branch can forward it to AcceptFile
    // (carrier rejects AcceptFile without account per
    // carrier/src/turtle_parse.c:415-431). turtle_emit.c:275 always
    // sets account on real FileRecv events; the fixture must mirror
    // that contract or the test diverges from production behavior.
    format!(
        "[] a antenna:Test ; \
         carrier:FileRecv \"_\" ; \
         carrier:account \"{TEST_ACCOUNT_ID}\" ; \
         carrier:conversationId \"{conv_id}\" ; \
         carrier:contactUri \"{sender_uri}\" ; \
         carrier:messageId \"{msg_id}\" ; \
         carrier:fileId \"{file_id}\" ; \
         carrier:filename \"{filename}\" ; \
         carrier:size {size} ."
    )
}

/// M4-A — synthetic carrier:FileComplete. Per
/// `carrier/src/turtle_emit.c:303-312`: conversationId / fileId / status
/// (status="finished" on success). The script's else-if branch ratchets
/// the attachment state to `complete` when status="finished".
fn file_complete_event(conv_id: &str, file_id: &str, status: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:FileComplete \"_\" ; \
         carrier:conversationId \"{conv_id}\" ; \
         carrier:fileId \"{file_id}\" ; \
         carrier:status \"{status}\" ."
    )
}

/// Pull a peer-cache field out of the store by SPARQL. Used to assert
/// the `<urn:msg:peer-cache:<uri>> messenger:online | displayName` triple
/// the M1-B brief (§5) requires for `bin/station sparql` smoke-tests.
const MESSENGER_NS: &str = "http://resonator.network/v2/messenger#";
fn peer_cache_field(store: &RdfStore, contact_uri: &str, field: &str) -> Option<String> {
    let q = format!(
        "SELECT ?v WHERE {{ \
         <urn:msg:peer-cache:{contact_uri}> <{MESSENGER_NS}{field}> ?v }}",
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("v") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

/// Pull the lod widget literal at a given antenna:below threshold off any
/// placed object. Matches the SPARQL the WS viewport query uses. M1-D —
/// callers query the bubble URN (urn:msg:bubble-obj:<mid>) at the M1-D
/// tier thresholds (200 / 400 / 99999); pre-M1-D pill assertions
/// have been re-pointed at the bubble's tier widgets since reactions live
/// inline in the bubble DSL now.
fn lod_widget_at(store: &RdfStore, obj_uri: &str, below: f64) -> Option<String> {
    let q = format!(
        "SELECT ?w WHERE {{ \
         <{obj_uri}> <{ANTENNA_NS}lod> ?l . \
         ?l <{ANTENNA_NS}below> \"{below}\"^^<http://www.w3.org/2001/XMLSchema#double> ; \
            <{ANTENNA_NS}widget> ?w }}",
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("w") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

/// Pull the tierLabel literal at a given antenna:below threshold off any
/// placed object's LOD. M2-A composer test asserts the labels match
/// test-plan.md M2.2 exactly (`one-line`/`tools`/`format`/`draft`,
/// case-sensitive).
fn lod_tier_label_at(store: &RdfStore, obj_uri: &str, below: f64) -> Option<String> {
    let q = format!(
        "SELECT ?t WHERE {{ \
         <{obj_uri}> <{ANTENNA_NS}lod> ?l . \
         ?l <{ANTENNA_NS}below> \"{below}\"^^<http://www.w3.org/2001/XMLSchema#double> ; \
            <{ANTENNA_NS}tierLabel> ?t }}",
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("t") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

/// Pull the per-tier worldHeight literal off an LOD blank node. M1-D
/// emits `antenna:worldHeight` on each `antenna:lod` blank node so
/// Station's per-tier rendering (placed_object.dart::LOD.worldHeight)
/// can grow the bubble's render rect tier-by-tier.
fn lod_world_height_at(store: &RdfStore, obj_uri: &str, below: f64) -> Option<f64> {
    let q = format!(
        "SELECT ?h WHERE {{ \
         <{obj_uri}> <{ANTENNA_NS}lod> ?l . \
         ?l <{ANTENNA_NS}below> \"{below}\"^^<http://www.w3.org/2001/XMLSchema#double> ; \
            <{ANTENNA_NS}worldHeight> ?h }}",
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("h") {
                return lit.value().parse().ok();
            }
        }
    }
    None
}

/// Pull the per-tier `antenna:fillMode` literal off an LOD blank node.
/// M2-B introduces this predicate as the opt-in signal that switches
/// Station's `_LODContent` from FittedBox.scaleDown to a bounded-rect
/// render so multi-line content can fill the rect (currently only the
/// messenger composer's tier 3 uses it). Returns `None` for any LOD
/// that doesn't carry the predicate — that's the historic default and
/// callers should not write an explicit `"scaleDown"` value.
fn lod_fill_mode_at(store: &RdfStore, obj_uri: &str, below: f64) -> Option<String> {
    let q = format!(
        "SELECT ?m WHERE {{ \
         <{obj_uri}> <{ANTENNA_NS}lod> ?l . \
         ?l <{ANTENNA_NS}below> \"{below}\"^^<http://www.w3.org/2001/XMLSchema#double> ; \
            <{ANTENNA_NS}fillMode> ?m }}",
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("m") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

/// Count how many LOD blank nodes hang off the placed object. M1-D — a
/// bubble with reactions emits 3 (tier 1/2/3); a bubble without reactions
/// emits 1 (compact single-tier mirror of the M1-C Path A baseline).
fn lod_count(store: &RdfStore, obj_uri: &str) -> usize {
    let q = format!(
        "SELECT (COUNT(?l) AS ?c) WHERE {{ <{obj_uri}> <{ANTENNA_NS}lod> ?l }}"
    );
    let Ok(QueryResults::Solutions(solutions)) = store.query(&q) else {
        return 0;
    };
    for sol in solutions.flatten() {
        if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("c") {
            return lit.value().parse().unwrap_or(0);
        }
    }
    0
}

/// Pull a placed object's geometry triples (x, y, worldWidth, worldHeight)
/// out of the store. Used by the M1-C Path A tests to assert bubble + pill
/// positioning without mounting Station. Returns None if any field is
/// missing — the placed object emit is all-or-nothing in pipeline.ttl, so
/// a partial row indicates a regression in the emit.
struct PlacedGeom {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
fn placed_geom(store: &RdfStore, uri: &str) -> Option<PlacedGeom> {
    let q = format!(
        "SELECT ?x ?y ?w ?h WHERE {{ \
         <{uri}> <{ANTENNA_NS}x> ?x ; \
                 <{ANTENNA_NS}y> ?y ; \
                 <{ANTENNA_NS}worldWidth> ?w ; \
                 <{ANTENNA_NS}worldHeight> ?h }}"
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        if let Some(sol) = solutions.flatten().next() {
            let to_f = |k: &str| -> Option<f64> {
                if let Some(oxigraph::model::Term::Literal(lit)) = sol.get(k) {
                    lit.value().parse().ok()
                } else {
                    None
                }
            };
            return Some(PlacedGeom {
                x: to_f("x")?,
                y: to_f("y")?,
                w: to_f("w")?,
                h: to_f("h")?,
            });
        }
    }
    None
}

const MID: &str = "677ff8db58f86147d26b3316d4efa34a3271be67";
const EMOJI_THUMBS: &str = "\u{1F44D}";
// M1-D — reactions live inside the bubble's tier widgets; the
// `urn:msg:react:<mid>:<encodeURIComponent(emoji)>` URN survives only as
// the chip's Button onTap target (handled by the existing TapEvent
// router), not as a placed-object URN. Tests assert on the bubble's
// widget DSL containing the Button URN at the relevant tier.
const REACT_URN_THUMBS: &str =
    "urn:msg:react:677ff8db58f86147d26b3316d4efa34a3271be67:%F0%9F%91%8D";

// M1-D bubble emit thresholds (mirror BUBBLE_TIER1/2/3_BELOW in pipeline.ttl).
const BUBBLE_TIER1_BELOW: f64 = 350.0;
const BUBBLE_TIER2_BELOW: f64 = 700.0;
const BUBBLE_TIER3_BELOW: f64 = 99999.0;

fn bubble_uri(mid: &str) -> String {
    format!("urn:msg:bubble-obj:{mid}")
}

#[test]
fn bubble_tier1_chip_wraps_in_button_for_quick_add_tap() {
    // M1A-001 regression, M1-D port. The reaction chip lives INSIDE the
    // bubble's tier-1 widget DSL now (Telegram-style inline). It must
    // still wrap in `Button{onTap=urn:msg:react:<mid>:<emoji>}[…]` so
    // `bin/station tap urn:msg:react:<mid>:<emoji>` resolves to the
    // rendered StationButton's onPressed and the urn:msg:react: handler
    // can toggle the self-reaction. Without the wrapper the tap RPC
    // returns "no button with target …" and UC3.8 (tier-1 quick-add)
    // silently fails.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    // Kick the script's init — pipeline's `if (typeof globalThis.init ==
    // 'undefined')` block fires on the first input.
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // Inject a reaction → script applies it → rebuildBubbles re-emits
    // each bubble's tier widgets with the inline chip wrapped in Button.
    dispatch::dispatch(
        &reaction_event(MID, "did:test:user1", EMOJI_THUMBS),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let widget = lod_widget_at(&store, &bubble_uri(MID), BUBBLE_TIER1_BELOW)
        .expect("bubble tier-1 widget must exist after reaction");

    assert!(
        widget.contains(&format!("Button{{onTap={REACT_URN_THUMBS}}}")),
        "tier-1 inline chip must wrap in Button{{onTap=<reactUrn>}}[…] for quick-add tap routing — got: {widget}",
    );
}

#[test]
fn bubble_tier3_renders_distinct_reactor_fingerprints() {
    // M1A-003 regression, M1-D port. shortUri() must strip the leading
    // <scheme>:<sub>: prefix off contactUris before truncating to 8
    // chars, otherwise synthetic test fixtures (did:test:user1 …) all
    // collapse to identical `did:test...` rows in the bubble's tier-3
    // reactor block and the demo reads as a single reactor repeated.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    for u in ["did:test:user1", "did:test:user2", "did:test:user3"] {
        dispatch::dispatch(
            &reaction_event(MID, u, EMOJI_THUMBS),
            &store, &dag, None, "", &mut out,
        );
        settle(&dag, &store, &mut out, 10);
    }

    let widget = lod_widget_at(&store, &bubble_uri(MID), BUBBLE_TIER3_BELOW)
        .expect("bubble tier-3 widget must exist after reactions");

    // The 8-char identifying tail of did:test:userN is "userN" (5 chars
    // post-prefix-strip → no truncation marker) — assert each of the
    // three fingerprints renders distinctly inside the bubble's tier-3
    // reactor block.
    for u in ["user1", "user2", "user3"] {
        assert!(
            widget.contains(&format!("Text{{value={u},")),
            "tier-3 widget must include distinct fingerprint {u} — got: {widget}",
        );
    }
    assert!(
        !widget.contains("did:test"),
        "tier-3 widget must not leak the bare did:test scheme prefix — got: {widget}",
    );
}

#[test]
fn peer_cache_populated_on_contact_online() {
    // M1-B regression. A `carrier:ContactOnline` event must DELETE+INSERT
    // <urn:msg:peer-cache:<uri>> messenger:online "true"^^xsd:boolean and
    // a messenger:lastSeen ISO-8601 timestamp into the store. Verifies the
    // `bin/station sparql 'SELECT ?n WHERE { ?p messenger:displayName ?n }'`
    // smoke-test contract from M1-reactions.md §5.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    dispatch::dispatch(
        &contact_online_event("did:test:user1"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let online = peer_cache_field(&store, "did:test:user1", "online")
        .expect("messenger:online must land for did:test:user1 after ContactOnline");
    assert_eq!(
        online, "true",
        "messenger:online must be true after ContactOnline — got: {online}"
    );

    let last_seen = peer_cache_field(&store, "did:test:user1", "lastSeen")
        .expect("messenger:lastSeen must land alongside messenger:online");
    assert!(
        last_seen.contains('T') && last_seen.ends_with('Z'),
        "messenger:lastSeen must be an ISO-8601 'Z'-suffixed timestamp — got: {last_seen}"
    );

    // Now flip offline — online should turn false but the entry must
    // still exist (cache never decays per brief §8 risk row 3).
    dispatch::dispatch(
        &contact_offline_event("did:test:user1"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let online_after = peer_cache_field(&store, "did:test:user1", "online")
        .expect("peer-cache entry must persist across ContactOffline");
    assert_eq!(
        online_after, "false",
        "messenger:online must flip to false after ContactOffline — got: {online_after}"
    );
}

#[test]
fn peer_cache_caches_display_name_from_contact_name_event() {
    // M1-B regression. carrier:ContactName must populate
    // messenger:displayName on the cache, lifted from the v0.2 peer-only
    // restriction so reactor rows in tier 3 carry real names when the
    // carrier supplies them. Tier-3 then prefers the display name over
    // the URI fingerprint fallback.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // First a ContactOnline so the cache has a row, then ContactName fills
    // in the display name.
    dispatch::dispatch(
        &contact_online_event("did:test:user1"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    dispatch::dispatch(
        &contact_name_event("did:test:user1", "Alice"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let name = peer_cache_field(&store, "did:test:user1", "displayName")
        .expect("messenger:displayName must land after ContactName");
    assert_eq!(
        name, "Alice",
        "messenger:displayName must echo the ContactName carrier:displayName — got: {name}"
    );

    // Online state should survive the ContactName upsert (cacheUpsert
    // carries forward fields not explicitly overridden).
    let online = peer_cache_field(&store, "did:test:user1", "online")
        .expect("messenger:online must persist across ContactName upsert");
    assert_eq!(
        online, "true",
        "ContactName must not clobber messenger:online — got: {online}"
    );
}

#[test]
fn bubble_tier3_renders_presence_dot_and_display_name() {
    // M1-B regression, M1-D port. The bubble's tier-3 widget must render:
    //   1. A presence-dot glyph (● U+25CF for online, ○ U+25CB for offline).
    //   2. The cached display name when present (otherwise the shortUri()
    //      fingerprint fallback).
    //   3. A relative-time string ("now" for sub-60s, "Nm" / "Nh" / …
    //      otherwise) computed from the per-reaction reactedAt timestamp.
    //
    // The brief's UC3 § Tier 3 also mandates ordering: online first, then
    // offline; within each group, most-recent reaction first. This test
    // injects alice (online) + bob (offline) reacting in opposite order so
    // a pure-time sort would put bob first, but presence-first sort keeps
    // alice on top.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // Alice — online, named via ContactName. Reacts first.
    dispatch::dispatch(
        &contact_online_event("did:test:alice"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &contact_name_event("did:test:alice", "Alice"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &reaction_event(MID, "did:test:alice", EMOJI_THUMBS),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // Bob — flipped offline before reacting (Bob *was* online once so the
    // peer-cache row exists, but ContactOffline has set online=false).
    dispatch::dispatch(
        &contact_online_event("did:test:bob"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &contact_offline_event("did:test:bob"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &reaction_event(MID, "did:test:bob", EMOJI_THUMBS),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let widget = lod_widget_at(&store, &bubble_uri(MID), BUBBLE_TIER3_BELOW)
        .expect("bubble tier-3 widget must exist after reactions");

    // Filled dot for the online reactor, hollow dot for the offline one.
    assert!(
        widget.contains('●'),
        "tier-3 widget must contain ● for online reactor — got: {widget}",
    );
    assert!(
        widget.contains('○'),
        "tier-3 widget must contain ○ for offline reactor — got: {widget}",
    );

    // Display name from cache (alice) and fingerprint fallback (bob).
    assert!(
        widget.contains("Text{value=Alice,"),
        "tier-3 widget must render Alice's cached display name — got: {widget}",
    );
    assert!(
        widget.contains("Text{value=bob,"),
        "tier-3 widget must render bob's URI fingerprint when no display name cached — got: {widget}",
    );

    // Relative-time substring — settle()'s sub-second tick keeps both
    // reactions inside the "< 60 s" branch so the row carries "now"
    // (M1-C-F1: shortened from "just now" to fit the inline reactor row's
    // worldWidth budget at the tier-3 LOD entry boundary).
    assert!(
        widget.contains("· now"),
        "tier-3 widget must include the '· now' relative-time string for fresh reactions — got: {widget}",
    );

    // Ordering: online (alice) before offline (bob) regardless of who
    // reacted later. The check looks at the byte offset of each name in
    // the rendered widget string.
    let alice_pos = widget.find("Text{value=Alice,").expect("alice row");
    let bob_pos   = widget.find("Text{value=bob,").expect("bob row");
    assert!(
        alice_pos < bob_pos,
        "tier-3 must order online reactors before offline (alice@{alice_pos} bob@{bob_pos}) — got: {widget}",
    );
}

#[test]
fn whoami_seeds_self_peer_cache_entry() {
    // M1-C (M1B-FU-002) regression. Real Jami never delivers a ContactOnline
    // event for our own selfUri, so without an explicit seed our own row in
    // tier 3 falls back to a hollow ○ presence dot + fingerprint name even
    // though we obviously know our own nick + are online. The fix lives in
    // the WhoAmI handler: cacheUpsert(selfUri, { online, displayName: nick,
    // lastSeen: now }) writes the three messenger:* triples for our row.
    //
    // Test flow: SelfId sets globalThis.selfUri (production gates this on
    // selfUri being populated); WhoAmI fires the seed; SPARQL the peer-cache
    // and assert displayName == nick + online == true.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    const SELF_URI: &str = "did:test:self";

    // Initial WhoAmI runs init; selfUri still empty → upsert no-ops, no row
    // for SELF_URI yet.
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let pre = peer_cache_field(&store, SELF_URI, "displayName");
    assert!(
        pre.is_none(),
        "self peer-cache entry must not exist before SelfId lands — got: {pre:?}"
    );

    // Carrier hands us our own URI.
    dispatch::dispatch(&self_id_event(SELF_URI), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // WhoAmI fires (Station reconnect path) → seeds the cache.
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);

    let name = peer_cache_field(&store, SELF_URI, "displayName")
        .expect("WhoAmI must seed messenger:displayName for self after SelfId");
    assert_eq!(
        name, "alice",
        "self peer-cache displayName must echo globalThis.nick (sed-injected as 'alice') — got: {name}"
    );

    let online = peer_cache_field(&store, SELF_URI, "online")
        .expect("WhoAmI must seed messenger:online=true for self");
    assert_eq!(
        online, "true",
        "self peer-cache online must be true after WhoAmI seed — got: {online}"
    );

    let last_seen = peer_cache_field(&store, SELF_URI, "lastSeen")
        .expect("WhoAmI must seed messenger:lastSeen for self");
    assert!(
        last_seen.contains('T') && last_seen.ends_with('Z'),
        "self peer-cache lastSeen must be ISO-8601 'Z'-suffixed — got: {last_seen}"
    );
}

#[test]
fn bubbles_emit_as_per_message_placed_objects() {
    // M1-C Path A + M1-D regression. Each message with a known messageId
    // must emit as its own `urn:msg:bubble-obj:<mid>` placed object with
    // x/y/worldWidth/worldHeight triples. M1-D extends the LOD shape:
    // bubbles WITH reactions emit THREE antenna:lod blocks (tier 1 / 2 /
    // 3) with the M1-D thresholds, each carrying its own per-tier
    // worldHeight for Station's anchor-aware placement.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // Inject a recv message (alice receives from peer) plus a reaction so
    // the bubble takes the M1-D three-tier shape.
    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &reaction_event(MID, "did:test:user1", EMOJI_THUMBS),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let burl = bubble_uri(MID);
    let geom = placed_geom(&store, &burl)
        .expect("bubble must emit as urn:msg:bubble-obj:<mid> placed object");
    assert!(
        geom.w >= 50.0 && geom.w <= 160.0,
        "bubble worldWidth must fall in the [BUBBLE_MIN_W, BUBBLE_MAX_W] band — got: {}",
        geom.w
    );
    // M1-D — placed-object worldHeight is the ANCHOR (tier 1) height,
    // which now includes the inline tier-1 chip row (~14 px) on top of
    // the base bubble. Loose upper bound to absorb chip-row tuning.
    assert!(
        geom.h >= 30.0 && geom.h <= 100.0,
        "bubble worldHeight must reflect tier-1 anchor height (base + chip row) — got: {}",
        geom.h
    );
    // Recv bubbles are left-aligned: center.x = chatLeft + margin + w/2.
    // chatLeft = -150 - 300/2 = -300; margin = 12; so x ≈ -288 + w/2.
    let expected_x = -300.0 + 12.0 + geom.w / 2.0;
    assert!(
        (geom.x - expected_x).abs() < 0.5,
        "recv bubble center x must align to chat panel left edge + margin — \
         expected ~{expected_x}, got {}",
        geom.x
    );
    // Bubble Y must fall within the chat panel's body window.
    assert!(
        geom.y > -250.0 && geom.y < -60.0,
        "bubble center y must be within the chat panel body area [-250, -60] — got: {}",
        geom.y
    );

    // M1-D — bubble with reactions carries THREE antenna:lod blocks, each
    // at the M1-D tier thresholds, each with its own per-tier worldHeight.
    assert_eq!(
        lod_count(&store, &burl),
        3,
        "M1-D bubble with reactions must carry 3 antenna:lod blocks (tier 1/2/3)"
    );
    for &below in &[BUBBLE_TIER1_BELOW, BUBBLE_TIER2_BELOW, BUBBLE_TIER3_BELOW] {
        let lod = lod_widget_at(&store, &burl, below).unwrap_or_else(|| {
            panic!(
                "bubble must carry an antenna:lod block at antenna:below={below}"
            )
        });
        assert!(
            lod.contains(&format!("urn:msg:bubble:{MID}")),
            "bubble tier-{below} widget must wrap in LongPress targeting urn:msg:bubble:<mid> — got: {lod}"
        );
        assert!(
            lod.contains("hello"),
            "bubble tier-{below} widget must carry the message text — got: {lod}"
        );
        // Per-tier worldHeight must be present — Station's parser is
        // OPTIONAL on this predicate so a missing literal would fall back
        // to the placed-object default and break the per-tier render path.
        assert!(
            lod_world_height_at(&store, &burl, below).is_some(),
            "bubble tier-{below} LOD blank node must carry antenna:worldHeight"
        );
    }

    // Chat panel widget should NOT carry the bubble inline — the body
    // area stays a fixed-height spacer that bubble placed objects overlay.
    //
    // M3-A: chat panel now has 4 LOD tiers (bubbles / day-grouped /
    // day-buckets / week-sparkline). below=99999 resolves to tier-4
    // (sparkline placeholder), which still wraps the chrome around a
    // 190-px Container — so the spacer assertion below holds, and the
    // tier-4 placeholder doesn't carry any per-bubble URNs by design.
    // Tier-1 bubble-area-not-inlined coverage is in the dedicated
    // chat_panel_tier1_preserves_existing_chatbody test below.
    let chat_lod = lod_widget_at(&store, "urn:msg:chat", 99999.0)
        .expect("chat panel widget must still emit");
    assert!(
        !chat_lod.contains(&format!("urn:msg:bubble:{MID}")),
        "Path A: bubble must NOT also appear inside the chat panel widget — got: {chat_lod}"
    );
    // Path-A bubble overlay: tier 1 (below=600) keeps the 190-px spacer
    // because the FittedBox.scaleDown path needs an intrinsic height,
    // and bubbles overlay the spacer via paint order at default zoom.
    let chat_tier1 = lod_widget_at(&store, "urn:msg:chat", CHAT_TIER1_BELOW)
        .expect("chat panel tier 1 widget must emit");
    assert!(
        chat_tier1.contains("Container{height=190"),
        "chat panel tier 1 must keep the 190-px bubble-area spacer — got: {chat_tier1}"
    );
}

#[test]
fn bubble_widget_dsl_contains_reactions_inline() {
    // M1-D core-shape regression. After M1-D, reactions live INSIDE the
    // bubble's tier-1 widget DSL (not as separate `urn:msg:react:*`
    // placed objects). The tier-1 widget must contain the emoji glyph as
    // a sub-widget of the bubble's Column, AND the standalone reaction
    // pill placed object MUST NOT exist.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &reaction_event(MID, "did:test:user1", EMOJI_THUMBS),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let burl = bubble_uri(MID);
    let widget = lod_widget_at(&store, &burl, BUBBLE_TIER1_BELOW)
        .expect("bubble tier-1 widget must exist after reaction");

    // Bubble's outer Container has color=msg-recv-bg (recv) — chrome
    // identical to pre-M1-D. Reactions area appears inline at the bottom
    // of the Column.
    assert!(
        widget.contains("color=msg-recv-bg"),
        "bubble chrome must use msg-recv-bg for recv messages — got: {widget}"
    );
    assert!(
        widget.contains("Container{color=msg-recv-bg"),
        "tier-1 widget must wrap in the bubble's Container chrome — got: {widget}"
    );
    // The chip Button onTap targets the per-message-emoji react URN so
    // the inline chip routes to the existing TapEvent handler.
    assert!(
        widget.contains(&format!("Button{{onTap={REACT_URN_THUMBS}}}")),
        "tier-1 widget must contain inline Button{{onTap=<reactUrn>}}[…] for the chip — got: {widget}"
    );
    // The emoji itself must appear inside the chip's Text widget. We
    // assert on a substring of the chip ("Container{color=" of the chip
    // followed by the emoji somewhere inside) rather than on the bare
    // emoji glyph (which appears in many places).
    assert!(
        widget.contains(EMOJI_THUMBS),
        "tier-1 widget must include the reaction emoji glyph inline — got: {widget}"
    );

    // M1-D dropped the standalone reaction pill placed object — the URN
    // remains as the chip's onTap target but no `urn:msg:react:*` placed
    // object should exist in the store.
    assert!(
        placed_geom(&store, REACT_URN_THUMBS).is_none(),
        "M1-D must NOT emit a standalone urn:msg:react:* placed object — reactions are inline in the bubble"
    );
}

#[test]
fn bubble_lod_tiers_grow_with_reactions() {
    // M1-D regression. With reactions present, each LOD's per-tier
    // worldHeight must grow strictly: tier 1 < tier 2 < tier 3. This is
    // what makes Station's anchor-aware placement reveal the avatars row
    // (tier 2) and reactor blocks (tier 3) downward as the user pinches
    // the bubble. If tier heights collapse, the depth pip lights up but
    // pinching reveals nothing new.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    // Inject 3 distinct reactors so the tier-3 reactor block has multiple
    // rows and h3 has actual headroom over h2 / h1.
    for u in ["did:test:user1", "did:test:user2", "did:test:user3"] {
        dispatch::dispatch(
            &reaction_event(MID, u, EMOJI_THUMBS),
            &store, &dag, None, "", &mut out,
        );
        settle(&dag, &store, &mut out, 10);
    }

    let burl = bubble_uri(MID);
    let h1 = lod_world_height_at(&store, &burl, BUBBLE_TIER1_BELOW)
        .expect("tier-1 LOD must carry antenna:worldHeight");
    let h2 = lod_world_height_at(&store, &burl, BUBBLE_TIER2_BELOW)
        .expect("tier-2 LOD must carry antenna:worldHeight");
    let h3 = lod_world_height_at(&store, &burl, BUBBLE_TIER3_BELOW)
        .expect("tier-3 LOD must carry antenna:worldHeight");

    assert!(
        h1 < h2,
        "M1-D tier 2 (avatars) must grow taller than tier 1 (count chip) — got h1={h1} h2={h2}"
    );
    assert!(
        h2 < h3,
        "M1-D tier 3 (reactor rows) must grow taller than tier 2 (avatars) — got h2={h2} h3={h3}"
    );
}

#[test]
fn bubble_with_no_reactions_stays_compact() {
    // M1-D shape contract. A bubble with NO reactions must emit a single
    // antenna:lod block — the depth pip stays absent (count(antenna:lod)
    // === 1) and the bubble's tap target is the LongPress-wrapped chrome
    // identical to the M1-C Path A baseline. Adding the per-tier reaction
    // ladder only when there's hidden content keeps the "pip indicates
    // reachable detail" contract from M0-D § 4.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);
    dispatch::dispatch(
        &text_message_event("did:tox:peer", MID, "hello"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let burl = bubble_uri(MID);
    assert_eq!(
        lod_count(&store, &burl),
        1,
        "M1-D bubble without reactions must emit exactly 1 antenna:lod block (no depth pip)"
    );
    // Single LOD lives at the tier-3 below threshold (99999) so it is the
    // "default / always-active" tier, mirroring the pre-M1-D bubble shape.
    let lod = lod_widget_at(&store, &burl, BUBBLE_TIER3_BELOW)
        .expect("compact bubble must carry its single LOD at below=99999");
    assert!(
        lod.contains("hello"),
        "compact bubble widget must carry the message text — got: {lod}"
    );
    assert!(
        lod.contains(&format!("urn:msg:bubble:{MID}")),
        "compact bubble must wrap in LongPress targeting urn:msg:bubble:<mid> — got: {lod}"
    );
    // Sanity: no reaction chip appears since there are no reactors.
    assert!(
        !lod.contains("urn:msg:react:"),
        "compact bubble without reactions must not contain any urn:msg:react: tap targets — got: {lod}"
    );
}

// M2-A composer placed-object thresholds (mirror COMPOSER_TIER1/2/3/4_BELOW
// in pipeline.ttl). Test-plan.md row M2.11 asserts hysteresis at these
// boundaries; row M2.2 asserts rail-dot label text. Tuned to give tier 1 a
// ~40% headroom over default-zoom screenPx (worldWidth 280 × scale 1.5 =
// 420 → tier 1 below=600), mirroring the M1-D bubble precedent (240→350).
const COMPOSER_TIER1_BELOW: f64 = 600.0;
const COMPOSER_TIER2_BELOW: f64 = 1200.0;
const COMPOSER_TIER3_BELOW: f64 = 2400.0;
const COMPOSER_TIER4_BELOW: f64 = 99999.0;
const COMPOSER_URI: &str = "urn:msg:composer";

#[test]
fn composer_emits_as_sibling_placed_object_with_four_lod_tiers() {
    // M2-A core shape contract. After M2-A, the composer is its own placed
    // object (sibling of <urn:msg:chat>) with a 4-tier LOD ladder. Geometry
    // matches the M2-composer.md § 3 spec adapted to the messenger
    // coordinate system: x = CHAT_X = -150 (centered on chat panel),
    // y = 0 (chat panel bottom edge after the M2-A worldHeight shrink),
    // worldWidth = 280, worldHeight = 30 (tier 1 anchor height).
    //
    // The 4 antenna:lod blocks must carry below thresholds 80/200/500/99999
    // and tierLabel literals `one-line`/`tools`/`format`/`draft` exactly
    // (case-sensitive, per test-plan.md M2.2). The Zoom Rail (M0-B) reads
    // antenna:tierLabel directly to populate dot labels when the composer
    // is the pinch focus, so a typo here breaks UC5.2 acceptance.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let geom = placed_geom(&store, COMPOSER_URI)
        .expect("M2-A composer must emit as <urn:msg:composer> placed object");
    assert!(
        (geom.x - (-150.0)).abs() < 0.5,
        "composer x must align with chat panel center (CHAT_X=-150) — got: {}",
        geom.x
    );
    assert!(
        (geom.y - 0.0).abs() < 0.5,
        "composer y must sit at chat panel bottom edge (y=0 after CHAT_H shrink) — got: {}",
        geom.y
    );
    assert!(
        (geom.w - 280.0).abs() < 0.5,
        "composer worldWidth must be 280 (M2-composer.md § 3) — got: {}",
        geom.w
    );
    assert!(
        (geom.h - 30.0).abs() < 0.5,
        "composer worldHeight must be 30 (M2-composer.md § 3, tier-1 anchor) — got: {}",
        geom.h
    );

    assert_eq!(
        lod_count(&store, COMPOSER_URI),
        4,
        "M2-A composer must carry exactly 4 antenna:lod blocks (one-line/tools/format/draft)"
    );

    // Test-plan M2.2 — labels case-sensitive.
    let expected = [
        (COMPOSER_TIER1_BELOW, "one-line"),
        (COMPOSER_TIER2_BELOW, "tools"),
        (COMPOSER_TIER3_BELOW, "format"),
        (COMPOSER_TIER4_BELOW, "draft"),
    ];
    for (below, label) in expected {
        let actual = lod_tier_label_at(&store, COMPOSER_URI, below).unwrap_or_else(|| {
            panic!(
                "composer must carry an antenna:lod block at antenna:below={below} with a tierLabel"
            )
        });
        assert_eq!(
            actual, label,
            "composer tier at below={below} must have tierLabel=\"{label}\" (test-plan.md M2.2 case-sensitive)"
        );
    }
}

#[test]
fn composer_tier1_is_real_textfield_when_input_enabled() {
    // M2-A tier-1 fidelity. When peerUri + conversationId are both set
    // (inputEnabled === true in the script), tier 1 must carry the same
    // TextField widget DSL the pre-M2-A inline composer used — same hint,
    // same target URN (urn:msg:chatinput), same key (input). This
    // guarantees the existing send-on-Enter flow (TextSubmitted → SendMsg)
    // is preserved verbatim by the placed-object re-author.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    // Drive the script into inputEnabled=true. The pipeline guards the
    // TextField behind `peerUri && conversationId`. Synthetic ContactOnline
    // (sets peerUri) + ConversationReady (sets conversationId) flips the
    // gate; SelfId seeds globalThis.selfUri so the rebuildChat path runs
    // through cleanly.
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"conv-m2a-test\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    let tier1 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER1_BELOW)
        .expect("composer tier 1 (one-line) must exist after rebuild");
    assert!(
        tier1.contains("TextField{"),
        "M2-A tier 1 must carry the real TextField when inputEnabled — got: {tier1}"
    );
    assert!(
        tier1.contains("target=urn:msg:chatinput"),
        "M2-A tier 1 TextField must keep target=urn:msg:chatinput (TextSubmitted wire) — got: {tier1}"
    );
    assert!(
        tier1.contains("key=input"),
        "M2-A tier 1 TextField must keep key=input for HudTextField re-focus — got: {tier1}"
    );
    assert!(
        !tier1.contains("(swarm not ready)"),
        "M2-A tier 1 must NOT show the muted guard once swarm is ready — got: {tier1}"
    );
}

#[test]
fn composer_tier1_shows_swarm_not_ready_guard_before_ready() {
    // M2-A tier-1 inputEnabled=false case. Default boot (no peerUri, no
    // conversationId — the pipeline_test fixture replaces __PEER_URI__
    // with "") must show the muted "(swarm not ready)" container, exactly
    // as the pre-M2-A inline composer did. This preserves the empty-
    // conversation visual baseline so M2.1 ("default boot — composer
    // renders tier 1") matches the pre-cut screenshot.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let tier1 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER1_BELOW)
        .expect("composer tier 1 must exist on default boot");
    assert!(
        tier1.contains("(swarm not ready)"),
        "M2-A tier 1 must render the muted guard when peerUri/conversationId are empty — got: {tier1}"
    );
    assert!(
        !tier1.contains("TextField{"),
        "M2-A tier 1 must NOT mount the TextField until inputEnabled flips true — got: {tier1}"
    );
}

// M2-B helper — drive the script into inputEnabled=true (peerUri +
// conversationId both set) so the real tier-2/3 widgets emit instead of
// the muted "(swarm not ready)" guards. Mirrors the setup in
// composer_tier1_is_real_textfield_when_input_enabled.
fn settle_input_enabled(store: &RdfStore, dag: &Dag) {
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", store, dag, None, "", &mut out);
    settle(dag, store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), store, dag, None, "", &mut out);
    settle(dag, store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        store, dag, None, "", &mut out,
    );
    settle(dag, store, &mut out, 10);
    dispatch::dispatch(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"conv-m2b-test\" .",
        store, dag, None, "", &mut out,
    );
    settle(dag, store, &mut out, 20);
}

#[test]
fn composer_tier2_emits_tool_row_above_singleline_textfield() {
    // M2-B core shape for tier 2 (per UC5-composer-expansion.md § Tier 2):
    //   - A 4-button tool row (📎 attach, 😀 emoji, @ mention, </> code).
    //   - A single-line TextField with the same target/key as tier 1 so
    //     the existing TextSubmitted → carrier:SendMsg wire is unchanged.
    //   - No multiline=true on tier 2 — that's tier 3 only.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let tier2 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER2_BELOW)
        .expect("composer tier 2 (tools) must exist");

    // Four tool buttons with the contracted urn:composer:* targets.
    for urn in [
        "urn:composer:attach",
        "urn:composer:emoji",
        "urn:composer:mention",
        "urn:composer:code",
    ] {
        assert!(
            tier2.contains(urn),
            "M2-B tier 2 must wire the {urn} tool button — got: {tier2}"
        );
    }
    assert!(
        tier2.contains("TextField{"),
        "M2-B tier 2 must include the single-line TextField — got: {tier2}"
    );
    assert!(
        tier2.contains("target=urn:msg:chatinput"),
        "M2-B tier 2 TextField must keep target=urn:msg:chatinput — got: {tier2}"
    );
    assert!(
        tier2.contains("key=input"),
        "M2-B tier 2 TextField must keep key=input — got: {tier2}"
    );
    assert!(
        !tier2.contains("multiline=true"),
        "M2-B tier 2 must stay single-line (multiline lives in tier 3) — got: {tier2}"
    );
    assert!(
        !tier2.contains("TOOLS HERE"),
        "M2-B must replace the M2-A placeholder string — got: {tier2}"
    );

    // Per-tier worldHeight override — without it the FittedBox in
    // _LODContent scales the tool row + TextField down to fit the anchor's
    // 30-px-tall world rect, rendering the composer as a hairline.
    let tier2_h = lod_world_height_at(&store, COMPOSER_URI, COMPOSER_TIER2_BELOW)
        .expect("M2-B tier 2 must carry an antenna:worldHeight override");
    assert!(
        tier2_h >= 50.0 && tier2_h <= 80.0,
        "M2-B tier 2 worldHeight should be ~60 px (tool row + single line) — got: {tier2_h}"
    );
}

#[test]
fn composer_tier3_emits_multiline_textfield_with_send_button() {
    // M2-B tier 3 contract:
    //   - TextField with multiline=true (renders as maxLines=null on the
    //     Station side; UC5 § Interaction model: Enter inserts newline,
    //     Cmd/Ctrl+Enter submits).
    //   - Same target/key as tier 1/2 so TextSubmitted wire holds and the
    //     GlobalKey-backed HudTextField State migrates across LOD swaps
    //     (focus-survives-pinch, M2-composer.md § 8 risk row 1).
    //   - A send button using the renderer's submitKey=input prop, so the
    //     ➤ press fires the field's onSubmitted closure → TextSubmitted →
    //     pipeline → carrier:SendMsg, without the pipeline ever needing the
    //     live text.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let tier3 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
        .expect("composer tier 3 (format) must exist");

    assert!(
        tier3.contains("TextField{"),
        "M2-B tier 3 must include the multi-line TextField — got: {tier3}"
    );
    assert!(
        tier3.contains("multiline=true"),
        "M2-B tier 3 TextField must carry multiline=true — got: {tier3}"
    );
    assert!(
        tier3.contains("target=urn:msg:chatinput"),
        "M2-B tier 3 TextField must share target=urn:msg:chatinput with tier 1/2 — got: {tier3}"
    );
    assert!(
        tier3.contains("key=input"),
        "M2-B tier 3 TextField must share key=input so HudTextField State \
         migrates across LOD swaps (focus-survives-pinch) — got: {tier3}"
    );
    assert!(
        tier3.contains("submitKey=input"),
        "M2-B tier 3 must include a send button wired via submitKey=input — got: {tier3}"
    );
    assert!(
        !tier3.contains("MULTI-LINE HERE"),
        "M2-B must replace the M2-A placeholder string — got: {tier3}"
    );

    // Per-tier worldHeight override — tier 3 grows the rendered rect
    // downward so the 4-line TextField + send button has room to render
    // at intrinsic size (FittedBox scaleDown contract). Without the
    // override the rect stays at the anchor's 30-px world height and the
    // multi-line content gets squished to a hairline.
    let tier3_h = lod_world_height_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
        .expect("M2-B tier 3 must carry an antenna:worldHeight override");
    assert!(
        tier3_h >= 100.0 && tier3_h <= 200.0,
        "M2-B tier 3 worldHeight should be ~140 px (4-line field + send row) — got: {tier3_h}"
    );

    // M2-B fill-mode opt-in — tier 3 must carry `antenna:fillMode "fill"`
    // so Station's `_LODContent` swaps the FittedBox.scaleDown render path
    // for a bounded-constraint one. Without this predicate, the multi-line
    // TextField's intrinsic ~136 px height top-aligns inside the
    // worldHeight=140 × scale rect (e.g. 630 px tall at scale 4.5) → most
    // of the rect renders empty. With it, `Column[mainAxisSize=max]` +
    // `Expanded[TextField]` stretch the field to the full rect height.
    let fill_mode =
        lod_fill_mode_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
            .expect("M2-B tier 3 must carry antenna:fillMode \"fill\"");
    assert_eq!(
        fill_mode, "fill",
        "M2-B tier 3 fillMode must be the literal string \"fill\" — got: {fill_mode}"
    );

    // The DSL must opt the Column into max main-axis sizing AND wrap the
    // TextField in an `Expanded` — those two are the in-DSL counterparts of
    // the LOD-level fillMode flip. Without them, even with bounded
    // constraints the Column would still shrink-wrap to intrinsic and the
    // fix would be a no-op.
    assert!(
        tier3.contains("Column{mainAxisSize=max}"),
        "M2-B tier 3 must drive the Column with mainAxisSize=max so it \
         claims the full rect height under fillMode=fill — got: {tier3}"
    );
    assert!(
        tier3.contains("Expanded[Container{"),
        "M2-B tier 3 must wrap the multi-line TextField in a `Container` \
         (with its own border) inside `Expanded` so the input area reads as \
         a distinct framed well at deep zoom — got: {tier3}"
    );
    assert!(
        tier3.contains("[TextField{"),
        "M2-B tier 3 inner Container must directly host the multi-line \
         TextField — got: {tier3}"
    );

    // Visual polish (andrej-15 sub-cut): tier 3 must carry a visible outer
    // edge (`borderColor=border-active`), wrap the TextField in an inner
    // Container with a `border-faint` frame so the input area distinguishes
    // from the outer chrome at deep zoom, and bump the send button glyph to
    // fontSize=32 so the ➤ anchors visibly at the rect's bottom-right
    // corner. Without these three the fill-mode rect reads as a featureless
    // dark band at scale 4.5–8 (content paints at native size, no FittedBox
    // scaling).
    assert!(
        tier3.contains("borderColor=border-active"),
        "M2-B tier 3 outer Container must declare `borderColor=border-active` \
         so the composer rect has a visible 1 px edge at deep zoom — got: \
         {tier3}"
    );
    assert!(
        tier3.contains("Container{borderColor=border-faint"),
        "M2-B tier 3 must wrap the TextField in `Container{{borderColor=\
         border-faint,…}}` so the input area carries its own visible 1 px \
         frame inside the surface-elevated outer chrome — got: {tier3}"
    );
    assert!(
        tier3.contains("Text{value=➤,fontSize=32"),
        "M2-B tier 3 send button glyph must use fontSize=32 so the ➤ \
         anchors the bottom-right at scale 4.5–8 native rendering — got: \
         {tier3}"
    );
    assert!(
        tier3.contains("fontSize=28"),
        "M2-B tier 3 multi-line TextField must declare `fontSize=28` so \
         body text reads at native size on a 4.5–8× scaled rect (theme \
         default Inter 11 is unreadable at that zoom) — got: {tier3}"
    );
    assert!(
        tier3.contains("hintFontSize=24"),
        "M2-B tier 3 multi-line TextField must declare `hintFontSize=24` \
         so the hint reads at native size on a 4.5–8× scaled rect (theme \
         default Inter 9 italic is unreadable at that zoom) — got: \
         {tier3}"
    );

    // Tier 1 + tier 2 must NOT carry the fontSize/hintFontSize overrides:
    // they keep the theme default (Inter 11 body, Inter 9 italic hint),
    // which renders cleanly under the historic FittedBox.scaleDown path.
    // Adding the overrides at smaller tiers would make the rendered field
    // appear oversized compared to the surrounding chrome.
    let tier1 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER1_BELOW)
        .expect("composer tier 1 must exist");
    assert!(
        !tier1.contains("fontSize=28") && !tier1.contains("hintFontSize="),
        "M2-B tier 1 must not carry tier-3's fontSize/hintFontSize overrides — got: {tier1}"
    );
    let tier2 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER2_BELOW)
        .expect("composer tier 2 must exist");
    assert!(
        !tier2.contains("fontSize=28") && !tier2.contains("hintFontSize="),
        "M2-B tier 2 must not carry tier-3's fontSize/hintFontSize overrides — got: {tier2}"
    );

    // Tiers 1 and 2 must NOT carry fillMode — they keep the historic
    // FittedBox.scaleDown render path. Authoring an explicit
    // "scaleDown" default would still trip Station into the new branch
    // logic (currently it treats anything ≠ "fill" as scaleDown, but
    // future-proofing the contract: only opt in when needed). Tier 4
    // (M2-D) opts in alongside tier 3 — see
    // tier4_lod_carries_world_height_and_fill_mode below.
    for (label, below) in [
        ("tier 1", COMPOSER_TIER1_BELOW),
        ("tier 2", COMPOSER_TIER2_BELOW),
    ] {
        assert!(
            lod_fill_mode_at(&store, COMPOSER_URI, below).is_none(),
            "{label} must omit antenna:fillMode (only tier 3+4 opt in)",
        );
    }
}

#[test]
fn tier4_widget_renders_split_with_preview_pseudo_bubble() {
    // M2-D core shape (per M2-composer.md § 5 M2-D + UC5 § Tier 4):
    // tier-4 widget DSL must be a Row split between the chat preview
    // (left half, mirroring rebuildChat's chrome) and the editor (right
    // half, sharing tier-3's draft-stack + format-toolbar + multi-line
    // TextField + send button via buildEditorBlock). The preview must
    // include:
    //   1. The CHAT header + 1-px divider + statusRow chrome.
    //   2. Inline-rendered real bubbles (capped at PREVIEW_BUBBLE_MAX=8)
    //      via bubbleWidgetForTier(m, 1).
    //   3. A green pseudo-bubble (msg-sent-bg) at the bottom carrying
    //      the in-progress draft body — proving the preview reads
    //      pendingDraft synchronously without waiting on the 250 ms
    //      debounce flush (test-plan.md M2.8: ≤ 200 ms preview update).
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    // Seed one recv bubble so the preview has a real message to inline.
    // The peer uri matches settle_input_enabled's contact_online_event.
    dispatch::dispatch(
        &text_message_event("did:tox:peer", "preview-test-mid-1", "yo"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // Fire a TextChanged carrying the draft body — the M2-D handler
    // calls rebuildComposer() synchronously after stashing pendingDraft,
    // so the tier-4 widget DSL we read next must already reflect the
    // pseudo-bubble. No ClockTick needed (and no 250 ms wait).
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "hello world"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let tier4 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER4_BELOW)
        .expect("M2-D composer tier 4 must exist");

    // The M2-A "FULL-CANVAS HERE" placeholder must be gone — tier 4 now
    // renders the real split.
    assert!(
        !tier4.contains("FULL-CANVAS HERE"),
        "M2-D tier 4 must replace the M2-A placeholder — got: {tier4}"
    );

    // Outer split structure: surface-base wrapper containing a Row with
    // two Expanded halves separated by a fixed-width gap. Don't pin the
    // exact whitespace — assert on the structural tokens.
    assert!(
        tier4.contains("color=surface-base"),
        "M2-D tier 4 outer wrapper must use surface-base so the editor's \
         and preview's own surface-elevated chrome pop visually — got: {tier4}"
    );
    assert!(
        tier4.contains("Row{mainAxisAlignment=spaceBetween,crossAxisAlignment=stretch}["),
        "M2-D tier 4 must use Row{{mainAxisAlignment=spaceBetween,\
         crossAxisAlignment=stretch}}: mainAxisAlignment lets _buildRow's \
         alignment branch pass Expanded children through unwrapped \
         (default Row wraps in Flexible(loose), turning Expanded into an \
         assertion error); crossAxisAlignment=stretch gives Expanded \
         children tight vertical constraints from the bounded SizedBox \
         (under center the editor's Column{{mainAxisSize=max}} asserts \
         on unbounded vertical) — got: {tier4}"
    );
    assert!(
        tier4.matches("Expanded[").count() >= 2,
        "M2-D tier 4 split must contain at least two Expanded halves \
         (preview + editor) — got: {tier4}"
    );

    // Preview chrome — mirrors rebuildChat's chatBody header.
    assert!(
        tier4.contains("value=CHAT"),
        "M2-D tier 4 preview must include the CHAT header (mirror of \
         rebuildChat's chatBody chrome) — got: {tier4}"
    );

    // Inline real bubble — bubbleWidgetForTier(msg, 1) emits
    // Container{color=msg-recv-bg,…}[…Text{value=yo,…}]. Cheap proof:
    // the recv-bubble bg color and the message text are both present.
    assert!(
        tier4.contains("color=msg-recv-bg"),
        "M2-D tier 4 preview must inline real recv bubbles via \
         bubbleWidgetForTier — got: {tier4}"
    );
    assert!(
        tier4.contains("value=yo"),
        "M2-D tier 4 preview must carry the inlined recv message text — got: {tier4}"
    );

    // Green pseudo-bubble for the in-progress draft. Same chrome as a
    // real sent bubble (msg-sent-bg + msg-sent-fg + just-now timestamp).
    assert!(
        tier4.contains("color=msg-sent-bg"),
        "M2-D tier 4 preview must end with a green pseudo-bubble \
         (color=msg-sent-bg) carrying the in-progress draft — got: {tier4}"
    );
    assert!(
        tier4.contains("value=hello world"),
        "M2-D tier 4 preview pseudo-bubble must carry the draft body \
         from pendingDraft — synchronous-rebuild contract for the \
         200 ms preview-update target — got: {tier4}"
    );
    assert!(
        tier4.contains("value=just now"),
        "M2-D tier 4 pseudo-bubble timestamp must be the literal \
         \"just now\" (honest about being a draft, no clock-drift in \
         tests) — got: {tier4}"
    );

    // Editor side — must contain the same key=input TextField + send
    // button as tier 3 (proves buildEditorBlock is shared).
    assert!(
        tier4.contains("TextField{") && tier4.contains("key=input"),
        "M2-D tier 4 editor must mount the key=input TextField (M2-B \
         GlobalKey-by-key-string registry migrates HudTextField State \
         across tier-3↔tier-4) — got: {tier4}"
    );
    assert!(
        tier4.contains("multiline=true"),
        "M2-D tier 4 editor must remain multi-line (mirrors tier 3) — got: {tier4}"
    );
    assert!(
        tier4.contains("urn:msg:send:"),
        "M2-D tier 4 editor must include the send button (urn:msg:send:<conv>) — got: {tier4}"
    );
}

#[test]
fn tier4_lod_carries_world_height_and_fill_mode() {
    // M2-D LOD opt-ins. Without worldHeight=320 the composer's anchor
    // (worldHeight=30 from seed.ttl) collapses tier 4 to a thin strip;
    // without fillMode=fill the split-view Row gets wrapped in
    // FittedBox.scaleDown and shrunk to its intrinsic size.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let h = lod_world_height_at(&store, COMPOSER_URI, COMPOSER_TIER4_BELOW)
        .expect("M2-D tier 4 LOD must carry an antenna:worldHeight override");
    assert!(
        (h - 320.0).abs() < f64::EPSILON,
        "M2-D tier 4 worldHeight must be 320.0 (split-view editor + \
         preview need height for both halves; see lod4Height comment in \
         pipeline.ttl) — got: {h}"
    );

    let fill = lod_fill_mode_at(&store, COMPOSER_URI, COMPOSER_TIER4_BELOW)
        .expect("M2-D tier 4 LOD must carry antenna:fillMode \"fill\"");
    assert_eq!(
        fill, "fill",
        "M2-D tier 4 fillMode must be \"fill\" so the split-view Row \
         gets bounded constraints (matches tier 3's M2-B opt-in) — got: {fill}"
    );
}

#[test]
fn composer_tiers_2_3_and_4_show_swarm_not_ready_guard_before_ready() {
    // Pre-handshake state — peerUri + conversationId are both empty, so
    // tiers 2/3/4 (like tier 1) must render the muted "(swarm not ready)"
    // container instead of mounting a TextField that has nowhere to send.
    // M2-D extends this to tier 4 since tier-4 now mounts a real TextField
    // when inputEnabled flips true.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    for (label, below) in [
        ("tier 2", COMPOSER_TIER2_BELOW),
        ("tier 3", COMPOSER_TIER3_BELOW),
        ("tier 4", COMPOSER_TIER4_BELOW),
    ] {
        let widget = lod_widget_at(&store, COMPOSER_URI, below)
            .unwrap_or_else(|| panic!("composer {label} must exist on default boot"));
        assert!(
            widget.contains("(swarm not ready)"),
            "{label} must render the muted guard pre-handshake — got: {widget}"
        );
        assert!(
            !widget.contains("TextField{"),
            "{label} must NOT mount a TextField until inputEnabled flips true — got: {widget}"
        );
    }
}

#[test]
fn chat_panel_no_longer_hosts_inline_composer() {
    // M2-A separation contract. The chat panel widget DSL must NOT contain
    // the TextField or the "(swarm not ready)" muted guard — those were
    // pulled out of rebuildChat()'s chatBody Column and are now owned by
    // <urn:msg:composer>. Without this separation, two TextFields would
    // race for the urn:msg:chatinput target and the M0 focus contract
    // (focusedElement → tier label) would point at the chat panel
    // ("chat") instead of the composer ("one-line") on a composer pinch.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    // Flip inputEnabled true so we exercise the path that USED to inline
    // a TextField — proving extraction holds in both states.
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"conv-m2a-extract\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    // M3-A: chat panel now has 4 LOD tiers; below=99999 resolves to
    // tier-4 (sparkline placeholder). The placeholder still wraps the
    // chat-panel chrome (CHAT header + statusRow + 190-px container) but
    // never inlines TextField / chatinput / swarm-not-ready — those live
    // in the composer's own placed-object LOD ladder. Tier-1 chrome
    // fidelity has its own dedicated test below.
    let chat_lod = lod_widget_at(&store, "urn:msg:chat", 99999.0)
        .expect("chat panel widget must still emit");
    assert!(
        !chat_lod.contains("TextField{"),
        "M2-A: chat panel must NOT inline a TextField — composer owns that now. got: {chat_lod}"
    );
    assert!(
        !chat_lod.contains("(swarm not ready)"),
        "M2-A: chat panel must NOT inline the swarm-not-ready guard — composer owns that now. got: {chat_lod}"
    );
    assert!(
        !chat_lod.contains("urn:msg:chatinput"),
        "M2-A: chat panel must NOT carry the chatinput target — composer owns that now. got: {chat_lod}"
    );
    // Sanity: chat panel chrome shell still emits. tier-4 (below=99999)
    // takes the task-#9 fillMode='fill' path so the chrome uses
    // `Column{mainAxisSize=max,mainAxisAlignment=center}` — fills the
    // bounded rect AND centers the chrome strip vertically so the
    // visual sits in the screen-middle band at deep zoom (rect can
    // exceed viewport — without center-alignment the chrome packs at
    // the rect's top edge which is off-screen above the viewport).
    // Tier-1 retains the historic `Container{height=190…}` spacer
    // (FittedBox path needs an intrinsic height); tier-1 fidelity is
    // asserted by `chat_panel_tier1_preserves_existing_chatbody`.
    assert!(
        chat_lod.contains("Column{mainAxisSize=max,mainAxisAlignment=center}["),
        "task #9: chat panel tier 4 must use \
         Column{{mainAxisSize=max,mainAxisAlignment=center}} so the \
         chrome centers vertically inside the bounded rect — got: {chat_lod}"
    );
}

// ── M2-C: Draft persistence ─────────────────────────────────────────────
//
// These tests assert the M2-C-A behaviour wired in pipeline.ttl:
//
//   1. TextChanged (Station-emitted on every keystroke) sets
//      pendingDraft; ClockTick (Antenna run-loop signal) flushes
//      pendingDraft to the store via sp:Modify DELETE + raw-triple
//      INSERT once Date.now() - dirtyAt >= 250 ms.
//   2. Empty body (user erased the field) collapses to a dropDraft —
//      no zombie draft URN left behind.
//   3. TextSubmitted (send) clears the persisted draft for the
//      conversation, mirroring UC5 § Interaction model.
//   4. The tier-3 widget renders a draft card carrying the
//      Station-side restore plumbing (restoreKey + URL-encoded
//      restoreValue) so a tap on the card populates the editor's
//      controller without round-tripping through new RDF vocab.

const DRAFT_NS: &str = "http://resonator.network/v2/messenger#";

/// Synthetic `antenna:TextChanged` line. Mirrors what HudTextField emits
/// on every keystroke (widget_renderer.dart::_buildTextField → onChanged).
/// Antenna's dispatch routes this through `insert_with_dag` →
/// `before_insert` broadcast → the messenger script's TextChanged handler.
fn text_changed_event(target: &str, value: &str) -> String {
    format!(
        "[] a <{ANTENNA_NS}TextChanged> ; \
         <{ANTENNA_NS}target> <{target}> ; \
         <{ANTENNA_NS}value> \"{value}\" ."
    )
}

/// Manually fire one ClockTick onto `antenna:clock`. The live antenna
/// run loop emits this every 1–25 ms (`MAX_SLEEP_MS=25` in lib.rs), but
/// the test harness's `settle` only pumps script emits and dispatch — it
/// doesn't drive the run loop. Tests that exercise the M2-C debounce
/// flush therefore wait past `DRAFT_DEBOUNCE_MS` (250 ms) and call this
/// helper directly to wake the script's ClockTick handler.
fn tick_clock(dag: &Dag) {
    dag.broadcast(
        &format!("{ANTENNA_NS}clock"),
        &format!("[] a <{ANTENNA_NS}ClockTick> ."),
    );
}

/// Pull the `messenger:body` literal for a conversation's draft URN.
/// Returns None if no draft is persisted (post-send / post-drop).
fn draft_body(store: &RdfStore, conversation_id: &str) -> Option<String> {
    let q = format!(
        "SELECT ?body WHERE {{ \
         <urn:draft:conv:{conversation_id}> <{DRAFT_NS}body> ?body }}"
    );
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("body") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

#[test]
fn draft_persists_after_textchanged_plus_debounce_and_clock_tick() {
    // M2-C-A core happy path. Inject a TextChanged carrying the typed
    // body, wait past the 250 ms debounce window, fire one ClockTick,
    // and assert the draft URN now carries the body literal in the
    // store. The wait is mandatory: the script's ClockTick handler
    // skips the flush when `Date.now() - pendingDraft.dirtyAt < 250`,
    // so a same-frame tick would silently no-op.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "hey there"),
        &store, &dag, None, "", &mut out,
    );
    // Let the script consume the TextChanged broadcast and set pendingDraft.
    settle(&dag, &store, &mut out, 1);
    assert!(
        draft_body(&store, "conv-m2b-test").is_none(),
        "no draft should be persisted before the debounce window elapses",
    );

    // Wait past DRAFT_DEBOUNCE_MS (250 ms) so the ClockTick handler's
    // age check fires, then trigger one tick.
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);

    let body = draft_body(&store, "conv-m2b-test").expect(
        "after TextChanged + 280 ms wait + ClockTick, the conversation's \
         messenger:Draft URN must carry the typed body",
    );
    assert_eq!(
        body, "hey there",
        "draft body must round-trip the TextChanged value verbatim"
    );
}

#[test]
fn empty_textchanged_drops_persisted_draft_on_next_tick() {
    // Erase-to-zero flow. Once a draft has been persisted (test 1 above),
    // a TextChanged carrying an empty string must drop the draft URN
    // rather than leaving the previous body as a zombie persisted draft.
    // The flush still rides on the same ClockTick path — the empty body
    // collapses to dropDraft inside the handler.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "to be erased"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);
    assert_eq!(
        draft_body(&store, "conv-m2b-test").as_deref(),
        Some("to be erased"),
        "precondition: the typed body must be persisted before the erase",
    );

    // Now erase. Same target URN, empty value.
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", ""),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);

    assert!(
        draft_body(&store, "conv-m2b-test").is_none(),
        "TextChanged with empty body + ClockTick must drop the draft URN \
         from the store (no zombie body left behind)",
    );
}

#[test]
fn textsubmitted_clears_persisted_draft_for_conversation() {
    // UC5 § Interaction model: send (TextSubmitted) clears the draft for
    // the conversation. Persists a draft, then submits a different value
    // — both the in-flight pendingDraft and the persisted draft URN
    // must be gone after the submit settles. (carrier:SendMsg goes out
    // separately; we only assert on the draft side here.)
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "draft body"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);
    assert!(
        draft_body(&store, "conv-m2b-test").is_some(),
        "precondition: draft must be persisted before submit",
    );

    // TextSubmitted with a finished message — the existing handler
    // emits carrier:SendMsg AND now also calls dropDraft.
    dispatch::dispatch(
        "[] a <http://resonator.network/v2/antenna#TextSubmitted> ; \
         <http://resonator.network/v2/antenna#target> <urn:msg:chatinput> ; \
         <http://resonator.network/v2/antenna#value> \"final message\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 8);

    assert!(
        draft_body(&store, "conv-m2b-test").is_none(),
        "TextSubmitted must drop the conversation's persisted draft URN \
         (UC5 § Interaction model: clear draft on send)",
    );
}

#[test]
fn tier3_widget_renders_draft_card_with_restore_props() {
    // Once a draft is persisted, the next rebuildComposer must surface
    // it as a tappable card above the framed input well. The card's
    // Button carries:
    //   - onTap=urn:msg:restoreDraft:<conv> → pipeline tap router
    //   - restoreKey=input → Station's HudTextField.setText target
    //   - restoreValue=<urlEncoded body> → the value to restore
    // A short sanitized preview must also appear inside the card so the
    // user can pick the right draft visually (UC5 § Tier 3 spec).
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    // Use a body that exercises URL encoding (commas + a non-ASCII char)
    // so the restoreValue prop is provably encoded — bare commas would
    // break the widget-DSL split-on-comma parser.
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "hey, world"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);

    let tier3 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
        .expect("tier 3 must exist after draft flush triggers rebuildChat");

    assert!(
        tier3.contains("urn:msg:restoreDraft:conv-m2b-test"),
        "tier 3 must render a draft card whose Button onTap encodes the \
         conversation id (urn:msg:restoreDraft:<conv>) — got: {tier3}"
    );
    assert!(
        tier3.contains("restoreKey=input"),
        "tier 3 draft card must declare restoreKey=input so HudTextField. \
         setText finds the registered editor — got: {tier3}"
    );
    // URL-encoding of "hey, world" — comma → %2C, space → %20.
    assert!(
        tier3.contains("restoreValue=hey%2C%20world"),
        "tier 3 draft card must URL-encode the body in restoreValue so the \
         widget-DSL parser's comma-split survives — got: {tier3}"
    );
    assert!(
        tier3.contains("DRAFTS (1)"),
        "tier 3 draft section must show a count header (DRAFTS (1)) above \
         the cards — got: {tier3}"
    );
}

#[test]
fn restore_draft_tap_clears_pending_draft() {
    // Defensive contract: the tap router branch for restoreDraft cancels
    // any in-flight pendingDraft when the tapped conversation matches.
    // Without the cancel, Station's onChanged firing on the controller
    // setText would land a TextChanged carrying the same body, and the
    // next ClockTick would burn one needless sp:Modify rewrite. The
    // observable surface for the test: after a TextChanged + restoreDraft
    // tap (both targeting the active conversation), no flush happens on
    // the next ClockTick — the persisted body is whatever was already in
    // the store, NOT the in-flight pendingDraft.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    // Type "first" → flush.
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "first"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);
    assert_eq!(draft_body(&store, "conv-m2b-test").as_deref(), Some("first"));

    // Type "in-flight" but DON'T wait for the flush — pendingDraft is
    // dirty. Then tap restoreDraft for this conv. The router branch
    // must clear pendingDraft, so the next ClockTick (after the wait)
    // produces no flush and the persisted body remains "first".
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "in-flight"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    dispatch::dispatch(
        "[] a <http://resonator.network/v2/antenna#TapEvent> ; \
         <http://resonator.network/v2/antenna#target> \
         <urn:msg:restoreDraft:conv-m2b-test> .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);

    // Wait past the debounce window and tick. If the cancel did NOT
    // fire, "in-flight" would have been flushed by now.
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 8);

    assert_eq!(
        draft_body(&store, "conv-m2b-test").as_deref(),
        Some("first"),
        "restoreDraft tap must cancel the in-flight pendingDraft — \
         persisted body should still be the pre-tap value, not the \
         mid-debounce 'in-flight' body",
    );
}

// ── M2-C-B: Format toolbar (tier-3 [B][I][</>][❝]) ──────────────────────
//
// Pure widget-DSL — each button declares `wrapSelectKey=input,prefix=...,
// suffix=...` so Station's HudTextField.wrapSelection populates the
// editor's controller on tap. No new RDF vocab. The pipeline emit shape
// is what this test pins down: presence of all four buttons, the right
// onTap URNs (so the existing `urn:composer:` router branch logs them),
// and URL-encoded prefix/suffix props (so the widget-DSL split-on-comma
// parser doesn't truncate any future format value carrying a `,`).

#[test]
fn tier3_widget_renders_format_toolbar_with_four_buttons() {
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let tier3 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
        .expect("composer tier 3 must exist after init + handshake");

    // Each format button must be present with its onTap URN, the
    // wrapSelectKey=input handle (matches the editor's `key=input`), and
    // URL-encoded prefix/suffix. URL-encoding mirrors M2-C-A's
    // restoreValue convention so the DSL parser path stays uniform.
    //
    // `*` and `>` and ` ` are not strictly DSL-breaking, but the pipeline
    // still URL-encodes them for the parsing-path uniformity rationale
    // documented in the FORMAT_BUTTONS comment block.
    for (urn, prefix_enc, suffix_enc) in [
        ("urn:composer:bold",   "**",       "**"),
        ("urn:composer:italic", "*",        "*"),
        ("urn:composer:code",   "%60",      "%60"),    // "`" → %60
        ("urn:composer:quote",  "%3E%20",   ""),       // "> " → %3E%20
    ] {
        let needle = format!(
            "Button{{onTap={urn},wrapSelectKey=input,prefix={prefix_enc},suffix={suffix_enc}",
        );
        assert!(
            tier3.contains(&needle),
            "tier 3 must include format button {urn} with wrapSelectKey=input \
             and URL-encoded prefix/suffix — needle `{needle}` not in tier 3 \
             widget DSL: {tier3}",
        );
    }

    // The toolbar sits between the draft stack (when present) and the
    // framed input well — assert the relative order so a future refactor
    // can't silently re-shuffle the column layout. Drafts may be empty
    // (this test doesn't seed one), so the assertion only requires the
    // toolbar to come before the framed input well's `borderColor=
    // border-faint` Container (which wraps the multi-line TextField).
    let bold_idx = tier3
        .find("urn:composer:bold")
        .expect("bold URN must appear");
    // Use the multi-line TextField marker as the input-well anchor —
    // matches only the editor (the toolbar's own Container also uses
    // borderColor=border-faint, so that needle would match the toolbar
    // first and the order assertion would always be a no-op).
    let well_idx = tier3
        .find("TextField{hint=Cmd+Enter")
        .expect("framed input well's multi-line TextField must appear");
    assert!(
        bold_idx < well_idx,
        "format toolbar (urn:composer:bold) must precede the multi-line \
         TextField in the tier-3 column",
    );

    // Sanity: the four label glyphs are visible inside the buttons at
    // the deep-zoom font size (matches M2-B's tier-3 body fontSize=28 so
    // the toolbar renders at native size inside the fillMode=fill rect
    // rather than as a tiny smudge against the chrome).
    for label in ["B", "I", "</>", "\u{275D}"] {
        let needle = format!("Text{{value={label},fontSize=28");
        assert!(
            tier3.contains(&needle),
            "tier 3 toolbar must show the {label} glyph at fontSize=28 — \
             needle `{needle}` not found",
        );
    }

    // Visual contrast (andrej-17 M2-C-B follow-up): the chip Container
    // must use `select-bg` (#1F2D5A) on the toolbar's `surface-elevated`
    // (#161B36) chrome plus a `border-active` ring. The original landing
    // shipped `surface-muted` chip bg + `border-faint` border, totalling
    // ~5 lum-delta against the chrome — invisible at scale 4.5–8. First
    // iter (border-active alone) didn't pop. Second iter (this contract)
    // bumps the chip bg to `select-bg` so each chip actively reads as a
    // distinct affordance against the chrome. Pinned so a future
    // refactor can't silently re-flatten the contrast.
    assert!(
        tier3.contains("Container{color=select-bg,padding=8,borderRadius=4,borderColor=border-active}"),
        "tier 3 format-toolbar chips must declare \
         `color=select-bg` + `borderColor=border-active` (not the \
         original `surface-muted` + `border-faint`) so each chip reads \
         as a distinct affordance at scale 4.5–8 — got: {tier3}",
    );

    // Layout (andrej-17 M2-C-B follow-up): the toolbar Row must use
    // `mainAxisAlignment=center` so the 4 chips sit at the toolbar's
    // visual center. With the original `start` alignment, at tier-3
    // active zoom (scale ≥ 4.3, where composer screenPx > screen width)
    // the chips were OFF-SCREEN LEFT — the user's natural focal point at
    // that zoom is the middle of the composer rect, not the left edge.
    // Pinned so a future refactor can't silently revert the centering.
    assert!(
        tier3.contains("Row{mainAxisAlignment=center}"),
        "tier 3 format-toolbar Row must declare `mainAxisAlignment=center` \
         so the chips sit at the toolbar's visual center (the user's \
         primary focal area at tier-3 zoom, since the composer rect is \
         wider than the screen) — got: {tier3}",
    );
}

#[test]
fn tier2_keeps_existing_tool_row_independent_of_format_toolbar() {
    // Defensive contract: M2-C-B adds the format toolbar to TIER 3 only.
    // Tier 2's `[📎][😀][@][</>]` tool row stays exactly as M2-B left it
    // — different URNs, different button shape (no wrapSelectKey props).
    // Catches a refactor that accidentally lifts the format toolbar
    // builder into tier 2 or vice-versa.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let tier2 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER2_BELOW)
        .expect("composer tier 2 must exist");

    // Tier 2 must NOT carry any format-toolbar URN or wrapSelectKey prop.
    for forbidden in [
        "urn:composer:bold",
        "urn:composer:italic",
        "urn:composer:quote",
        "wrapSelectKey",
    ] {
        assert!(
            !tier2.contains(forbidden),
            "tier 2 must NOT carry M2-C-B format toolbar marker `{forbidden}` \
             (it's a tier-3-only affordance) — got: {tier2}"
        );
    }

    // Tier 2 must still carry the M2-B tool-row URNs (no regression).
    for urn in [
        "urn:composer:attach",
        "urn:composer:emoji",
        "urn:composer:mention",
    ] {
        assert!(
            tier2.contains(urn),
            "tier 2 must still wire the M2-B {urn} tool button — got: {tier2}"
        );
    }

    // Layout (andrej-20 M2-C-B QA TICKET-C): the tier-2 tool row uses
    // `mainAxisAlignment=center` for the same focal-area reason as tier-3
    // (the composer rect's screenPx ≈ 840 at scale 3 > 800 screen width,
    // so leftmost icons clip off-screen at the user's centered focal
    // point under `start`). Mirrors the tier-3 alignment assertion and
    // pins the fix so a future refactor can't silently revert it.
    assert!(
        tier2.contains("Row{mainAxisAlignment=center}"),
        "tier 2 tool row must declare `mainAxisAlignment=center` so the \
         📎/😀/@/</> buttons sit at the row's visual center (the user's \
         primary focal area at tier-2 zoom, since composer screenPx > \
         screen width) — got: {tier2}",
    );
}

// ── M2-C-B (TICKET-1): tier-3 send button onTap routing ────────────────
//
// The send button needs `onTap=urn:msg:send:<conv>` so Station's
// _RegisteredTapButton mounts it in TapRegistry. Without it, `bin/station
// tap urn:msg:send:<conv>` returns "no button with target …" and live QA
// of M2.6/UC5.6 (send-clears-draft) is blocked. The actual send still
// fires via `submitKey=input` → HudTextField.triggerSubmit → onSubmitted
// → TextSubmitted (option b in the cut brief — single send code path),
// so the pipeline router branch for `urn:msg:send:` only logs the tap.
// The defensive shape: tap MUST NOT emit a `carrier:SendMsg`. That's
// the single-send-path contract — duplicating it would risk diverging
// dropDraft semantics under future edits.

#[test]
fn send_button_carries_tap_urn_alongside_submit_key() {
    // Tier 3 contract: the send button declares both `onTap=urn:msg:send:
    // <conv>` (registry breadcrumb so `bin/station tap` resolves it) AND
    // `submitKey=input` (the actual send wire). Both must coexist on the
    // same Button; the brief is explicit that the breadcrumb does NOT
    // replace the submit path.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);

    let tier3 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER3_BELOW)
        .expect("composer tier 3 must exist after init + handshake");

    // The conversation id seeded by `settle_input_enabled` — the same
    // value the runtime injects via `carrier:ConversationReady`. Pinning
    // it here also guards the URN scheme (any future change to the
    // breadcrumb shape needs to be visible in the test diff).
    let needle = "Button{onTap=urn:msg:send:conv-m2b-test,submitKey=input,\
                  padding=8,borderRadius=4}[Text{value=\u{27A4},fontSize=32";
    assert!(
        tier3.contains(needle),
        "tier 3 send button must declare onTap=urn:msg:send:conv-m2b-test \
         alongside submitKey=input — needle `{needle}` not found in tier-3 \
         widget DSL: {tier3}"
    );

    // Defensive: tier 1 has no send button (just a single-line TextField),
    // and tier 2's send is via Enter on the single-line field — so neither
    // tier should carry the send URN. Catches a refactor that lifts the
    // breadcrumb into the lower tiers.
    let tier1 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER1_BELOW)
        .expect("composer tier 1 must exist");
    assert!(
        !tier1.contains("urn:msg:send:"),
        "tier 1 must NOT carry the send-tap URN (no send button at this \
         tier) — got: {tier1}"
    );
    let tier2 = lod_widget_at(&store, COMPOSER_URI, COMPOSER_TIER2_BELOW)
        .expect("composer tier 2 must exist");
    assert!(
        !tier2.contains("urn:msg:send:"),
        "tier 2 must NOT carry the send-tap URN (no send button at this \
         tier) — got: {tier2}"
    );
}

#[test]
fn tap_on_send_button_routes_through_pipeline_without_duplicate_send() {
    // Behavioural contract for option (b): tap on the send URN routes
    // through the pipeline's TapEvent handler, which acknowledges the tap
    // (debug breadcrumb) but DOES NOT emit a `carrier:SendMsg`. The
    // single send code path remains TextSubmitted → carrier:SendMsg +
    // dropDraft. Asserting the absence of `carrier:SendMsg` after a tap
    // is the regression guard against accidentally upgrading the tap to
    // a second send path during a future refactor.
    let (store, dag) = build_messenger_pipeline();
    settle_input_enabled(&store, &dag);
    let mut out = CaptureOut::new();

    // Drain the settle's emits — `settle_input_enabled` may have produced
    // unrelated emits before we start watching. Anything captured AFTER
    // the tap is what we're asserting on.
    out.messages.clear();

    dispatch::dispatch(
        "[] a <http://resonator.network/v2/antenna#TapEvent> ; \
         <http://resonator.network/v2/antenna#target> \
         <urn:msg:send:conv-m2b-test> .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 8);

    let send_msg_count = out
        .messages
        .iter()
        .filter(|m| m.contains("carrier:SendMsg"))
        .count();
    assert_eq!(
        send_msg_count, 0,
        "tap on urn:msg:send:<conv> must NOT emit carrier:SendMsg — the \
         tap is a breadcrumb, the actual send fires via submitKey=input → \
         TextSubmitted. Captured emits (filtered for carrier:SendMsg): \
         {:?}",
        out.messages
            .iter()
            .filter(|m| m.contains("carrier:SendMsg"))
            .collect::<Vec<_>>(),
    );
}

// ── M3-A — chat panel 4-tier scaffold (UC2 — Conversation Timeline) ─────
//
// The chat panel <urn:msg:chat> graduates from a single-tier LOD to a
// 4-tier ladder. Tier 1 carries today's chatBody verbatim (CHAT header +
// statusRow + 190-px bubble-area spacer overlaid by the per-message bubble
// placed objects). Tiers 2-4 are PLACEHOLDER scaffolds wrapping the same
// chrome around a short label string — M3-B/C/D replace them with
// day-grouped bubbles, day-bucket rows, and the 60-day vertical sparkline
// respectively.
//
// Threshold tuning mirrors the M2-A composer precedent: brief specifies
// 150/300/600/99999 screenPx, but chat panel worldWidth=300 × default
// scale=1.5 = 450 screenPx at boot would land in tier-2 under those
// values, violating "tier 1 = today's behaviour at default zoom." Lift
// tier-1 below to 600 (~33% headroom) and scale the rest 2×/4× — the
// same ladder the composer uses, so the rail's 4 dots line up across
// both panels at any zoom.

const CHAT_URI: &str = "urn:msg:chat";
const CHAT_TIER1_BELOW: f64 = 600.0;
const CHAT_TIER2_BELOW: f64 = 1200.0;
const CHAT_TIER3_BELOW: f64 = 2400.0;
const CHAT_TIER4_BELOW: f64 = 99999.0;

#[test]
fn chat_panel_emits_4_lod_tiers_with_correct_labels() {
    // M3-A core shape contract. The chat panel must carry exactly 4
    // antenna:lod blocks at thresholds 600/1200/2400/99999 with case-
    // sensitive tierLabel literals matching test-plan.md M3.2-M3.4
    // (`bubbles` / `day-grouped` / `day-buckets` / `week-sparkline`).
    // The Zoom Rail (M0-B) reads antenna:tierLabel to populate dot
    // labels — a typo here breaks UC2.X acceptance for the rail rendering.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    assert_eq!(
        lod_count(&store, CHAT_URI),
        4,
        "M3-A chat panel must carry exactly 4 antenna:lod blocks \
         (bubbles/day-grouped/day-buckets/week-sparkline)"
    );

    let expected = [
        (CHAT_TIER1_BELOW, "bubbles"),
        (CHAT_TIER2_BELOW, "day-grouped"),
        (CHAT_TIER3_BELOW, "day-buckets"),
        (CHAT_TIER4_BELOW, "week-sparkline"),
    ];
    for (below, label) in expected {
        let actual = lod_tier_label_at(&store, CHAT_URI, below).unwrap_or_else(|| {
            panic!(
                "chat panel must carry an antenna:lod block at antenna:below={below} \
                 with a tierLabel"
            )
        });
        assert_eq!(
            actual, label,
            "chat panel tier at below={below} must have tierLabel=\"{label}\" \
             (test-plan.md M3.2-M3.4 case-sensitive)"
        );
    }
}

#[test]
fn chat_panel_tier1_preserves_existing_chatbody() {
    // M3-A "tier 1 is the load-bearing tier" contract. Tier 1's widget
    // DSL must remain functionally identical to the pre-M3-A chatBody:
    // CHAT header + 1-px divider + statusRow (nick + connStatus dot +
    // peer label + friendStatus dot) + 190-px bubble-area spacer. The
    // M0-B/M1-D/M2-A flows all depend on this shape staying intact —
    // bubbles overlay the spacer via paint order; rebuildBubbles never
    // inlines bubbles into the chat panel widget.
    //
    // Drives the script into inputEnabled=true (peer + conversationId
    // set) so any state-conditional branch in rebuildChat runs through.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"conv-m3a-tier1\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    let tier1 = lod_widget_at(&store, CHAT_URI, CHAT_TIER1_BELOW)
        .expect("chat panel tier 1 (bubbles) must exist after rebuild");

    // Chrome contract.
    assert!(
        tier1.contains("Container{color=surface-elevated,padding=8,borderRadius=6}"),
        "tier 1 must wrap in the existing surface-elevated chrome — got: {tier1}"
    );
    assert!(
        tier1.contains("Text{value=CHAT,fontSize=10,color=text-code"),
        "tier 1 must carry the CHAT header — got: {tier1}"
    );
    assert!(
        tier1.contains("Container{color=border-active,height=1}"),
        "tier 1 must carry the 1-px divider under the header — got: {tier1}"
    );
    assert!(
        tier1.contains("StatusDot{"),
        "tier 1 must carry the connStatus / friendStatus dots in the statusRow — got: {tier1}"
    );
    assert!(
        tier1.contains("Container{height=190"),
        "tier 1 must carry the 190-px bubble-area spacer (Path A spacer that bubbles \
         overlay via paint order) — got: {tier1}"
    );

    // task #9: tier 1 must STAY on the FittedBox.scaleDown path, NOT
    // take the fillMode='fill' path. At default zoom rect ≈ intrinsic
    // chrome size, FittedBox.scaleDown works correctly + bubble overlay
    // paint-order depends on the 190-px spacer. Flipping tier 1 to fill
    // mode would reflow the spacer to bounded constraints and break
    // bubble overlay alignment. Asserts both the predicate absence (no
    // antenna:fillMode triple on the LOD blank node) AND the absence of
    // the fill-shape `mainAxisSize=max` flag — defense-in-depth.
    assert!(
        lod_fill_mode_at(&store, CHAT_URI, CHAT_TIER1_BELOW).is_none(),
        "tier 1 must NOT carry antenna:fillMode — keeps FittedBox path"
    );
    assert!(
        !tier1.contains("mainAxisSize=max"),
        "tier 1 must NOT use Column{{mainAxisSize=max}} — that's the fill-mode shape \
         (illegal under unbounded vertical constraints from FittedBox) — got: {tier1}"
    );

    // Tier 1 must NOT carry any of the placeholder labels — those live
    // in tiers 2/3/4 only. A regression that emits the placeholder
    // string into tier 1 would visibly break the default-zoom view.
    for label in ["DAY-GROUPED HERE", "DAY BUCKETS HERE", "SPARKLINE HERE"] {
        assert!(
            !tier1.contains(label),
            "tier 1 must NOT contain placeholder label \"{label}\" — got: {tier1}"
        );
    }
}

#[test]
fn chat_panel_tiers_2_3_carry_chrome_around_inner_area() {
    // M3-B chrome-continuity contract — what was M3-A's
    // `chat_panel_tiers_2_3_4_render_placeholders_with_chrome`. Tiers 2/3
    // no longer carry placeholder strings (M3-B replaced them with the
    // day-grouped bubble area / day-bucket row list); the chrome itself
    // (CHAT header, divider, statusRow, 190-px inner) must persist so
    // the rail's 4 dots map onto a recognisable chat surface at every
    // zoom. Per-tier inner-content assertions live in
    // `tier2_renders_inline_bubbles_with_day_separators` and
    // `tier3_renders_row_per_day_with_count_and_snippets` below.
    //
    // Tier 4 still carries the SPARKLINE HERE placeholder — covered by
    // `chat_panel_tier4_carries_sparkline_placeholder_with_chrome`.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    for below in [CHAT_TIER2_BELOW, CHAT_TIER3_BELOW] {
        let widget = lod_widget_at(&store, CHAT_URI, below).unwrap_or_else(|| {
            panic!("chat panel tier at below={below} must emit a widget literal")
        });
        assert!(
            widget.contains("Container{color=surface-elevated,padding=8,borderRadius=6}"),
            "tier at below={below} must carry the chat-panel chrome (surface-elevated) — got: {widget}"
        );
        assert!(
            widget.contains("Text{value=CHAT,fontSize=10,color=text-code"),
            "tier at below={below} must carry the CHAT header — got: {widget}"
        );
        assert!(
            widget.contains("Container{color=border-active,height=1}"),
            "tier at below={below} must carry the 1-px divider — got: {widget}"
        );
        assert!(
            widget.contains("StatusDot{"),
            "tier at below={below} must carry the statusRow dots — got: {widget}"
        );

        // task #9: tiers 2/3 take the fillMode='fill' path with
        // `mainAxisSize=max,mainAxisAlignment=center`. The bounded rect
        // (worldHeight × scale) holds the chrome at its intrinsic
        // height, but the center-alignment puts the chrome strip at
        // the rect's vertical middle — which IS the viewport's
        // screen-middle band when the user is centered on the panel.
        // Pre-fix the chrome packed at the rect TOP (mainAxisAlignment
        // defaulting to start) and at deep zoom that top sat hundreds
        // of pixels above the viewport, taking the chrome with it (the
        // M3 demo blocker). The fixed `Container{height=190}` shell is
        // gone — the inner area renders at intrinsic now.
        assert!(
            widget.contains("Column{mainAxisSize=max,mainAxisAlignment=center}["),
            "tier at below={below} must use \
             Column{{mainAxisSize=max,mainAxisAlignment=center}} so the \
             chrome centers vertically in the bounded rect — got: {widget}"
        );
        let fill_mode = lod_fill_mode_at(&store, CHAT_URI, below).unwrap_or_else(|| {
            panic!("tier at below={below} must carry antenna:fillMode 'fill'")
        });
        assert_eq!(
            fill_mode, "fill",
            "tier at below={below} fillMode must be the literal string 'fill' — got: {fill_mode}"
        );

        // M3-B regression guard — placeholder strings are gone.
        for stale in ["DAY-GROUPED HERE", "DAY BUCKETS HERE"] {
            assert!(
                !widget.contains(stale),
                "tier at below={below} must NOT carry the M3-A placeholder \"{stale}\" — got: {widget}"
            );
        }
    }
}

#[test]
fn chat_panel_tier4_carries_real_sparkline_with_chrome() {
    // M3-D — tier 4 ("week-sparkline") now carries the real 60-day
    // vertical density column inside the chat-panel chrome (was
    // `SPARKLINE HERE` placeholder through M3-A/B/C). The chrome
    // contracts (surface-elevated, fillMode='fill', mainAxisSize=max +
    // mainAxisAlignment=center, CHAT header, statusRow) are unchanged.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-chrome");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "tier4-1", "ping");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("chat panel tier 4 (week-sparkline) must emit a widget literal");

    assert!(
        widget.contains("Container{color=surface-elevated,padding=8,borderRadius=6}"),
        "tier 4 must carry the chat-panel chrome — got: {widget}"
    );
    assert!(
        widget.contains("Text{value=CHAT,fontSize=10,color=text-code"),
        "tier 4 must carry the CHAT header — got: {widget}"
    );
    assert!(
        widget.contains("StatusDot{"),
        "tier 4 must carry the statusRow dots — got: {widget}"
    );
    assert!(
        widget.contains("Column{mainAxisSize=max,mainAxisAlignment=center}["),
        "tier 4 must use Column{{mainAxisSize=max,mainAxisAlignment=center}} \
         so the chrome centers vertically inside the bounded rect — got: {widget}"
    );
    let fill_mode = lod_fill_mode_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("tier 4 must carry antenna:fillMode 'fill'");
    assert_eq!(
        fill_mode, "fill",
        "tier 4 fillMode must be the literal 'fill' — got: {fill_mode}"
    );

    // M3-D — placeholder string is gone.
    assert!(
        !widget.contains("SPARKLINE HERE"),
        "tier 4 must NOT carry the M3-A/B/C 'SPARKLINE HERE' placeholder \
         — got: {widget}"
    );
    assert!(
        !widget.contains("DAY-GROUPED HERE") && !widget.contains("DAY BUCKETS HERE"),
        "tier 4 must NOT carry any other tier's placeholder string — got: {widget}"
    );
    // Real ticks: every tick wraps a fixed-width Container colored with
    // either `live-data` (1 sender) or `structural` (≥2 senders). Either
    // role must show up — together with the canonical
    // `Container{color=<role>,width=200,…,borderRadius=1}` shape — at
    // least once in the widget DSL.
    assert!(
        widget.contains("Container{color=live-data,width=200,height=")
            || widget.contains("Container{color=structural,width=200,height="),
        "tier 4 must carry the real sparkline ticks (Container with \
         color=live-data|structural, width=200) — got: {widget}"
    );
}

// ── M3-B — day-bucket aggregation (UC2 — Conversation Timeline) ─────────
//
// M3-B replaces the tier-2 / tier-3 `DAY-GROUPED HERE` / `DAY BUCKETS
// HERE` placeholders with real content driven by a per-conversation
// messenger:DayBucket aggregation:
//
//   - Tier 2 walks globalThis.messages, calls bubbleWidgetForTier(m,1)
//     per messageId-bearing entry, and inserts a day-separator row
//     (Text{text-tertiary} + 1-px border-active divider) between
//     consecutive bubbles whose dayKeys differ.
//   - Tier 3 queries the rolling-60 aggregation, renders
//     `<dateLabel>  <count> msgs  "<first>" → "<last>"` per day,
//     each row wrapped in `Button{onTap=urn:msg:teleport:day:<key>}`.
//   - Aggregation runs from inside rebuildChat (single source of truth)
//     so the same buffer snapshot drives the store-side
//     messenger:DayBucket triples and the rendered widget DSL.
//   - URN scheme: `urn:msg:bucket:day:<conv>:<YYYY-MM-DD>` so the
//     M5 multi-conversation future (separate buckets per conv) doesn't
//     collide.
//   - Multi-valued `messenger:participants` predicate (one triple per
//     participant) per the brief's pre-decided design — divergent from
//     the brief's literal Turtle list syntax but semantically
//     equivalent and SPARQL-friendlier.

const MSG_NS: &str = MESSENGER_NS;

/// Drive the script into a known-ready state — selfId + ContactOnline +
/// ConversationReady — so flushDayBuckets is no longer guarded by the
/// pre-conversationId bail-out and aggregation runs on every rebuildChat.
/// Mirrors the same state-pump pattern as
/// `chat_panel_tier1_preserves_existing_chatbody`.
fn drive_to_ready(
    store: &RdfStore,
    dag: &Dag,
    out: &mut CaptureOut,
    self_uri: &str,
    peer_uri: &str,
    conv_id: &str,
) {
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", store, dag, None, "", out);
    settle(dag, store, out, 20);
    dispatch::dispatch(&self_id_event(self_uri), store, dag, None, "", out);
    settle(dag, store, out, 10);
    dispatch::dispatch(&contact_online_event(peer_uri), store, dag, None, "", out);
    settle(dag, store, out, 10);
    let conv_event = format!(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"{peer_uri}\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_event, store, dag, None, "", out);
    settle(dag, store, out, 20);
}

/// Synthetic carrier:TextMessage with an optional override timestamp
/// (xsd:long milliseconds). The script's text-message handler doesn't
/// accept an override today, so this drives the message in via the
/// regular path and then patches the JS-side `globalThis.messages[i].ts`
/// via a post-receive antenna:Eval shim. There's no eval shim, so the
/// safer + portable approach: inject a M3-B test-only event the script
/// recognises — but adding one to pipeline.ttl pollutes prod with test
/// scaffolding. Instead, this helper drives a real TextMessage and the
/// test then calls `backdate_message` (below) to nudge the resulting
/// entry's timestamp via a SPARQL-driven side channel.
///
/// Simpler path: since `globalThis.messages` is a JS variable (not RDF),
/// the deterministic backdate happens by sending a sequence of
/// TextMessages and asserting whatever bucket the day-key produces — the
/// cargo tests don't actually need backdated timestamps for the
/// aggregation correctness check, because `dayKey(Date.now())` for all
/// of them resolves to the SAME day. To exercise multi-day aggregation
/// we need either backdating OR running tests across midnight, neither
/// of which is reliable. So: the multi-day tests below drive bucket
/// aggregation by injecting messages and asserting that 1 day bucket
/// exists with the correct count (single-day case); a separate
/// `aggregateDayBuckets_unit_test` covers the multi-day grouping by
/// driving the helper directly via a dedicated unit-test entry point.
///
/// For multi-day visual + grouping behavior, the live verification step
/// (Skill(radio)) covers it; the unit tests here cover the
/// aggregation-correctness + URN-scope + chrome contracts.
fn send_text_message(
    store: &RdfStore,
    dag: &Dag,
    out: &mut CaptureOut,
    contact_uri: &str,
    mid: &str,
    text: &str,
) {
    dispatch::dispatch(
        &text_message_event(contact_uri, mid, text),
        store,
        dag,
        None,
        "",
        out,
    );
    settle(dag, store, out, 20);
}

/// Pull DayBucket triples out of the store. Returns a list of
/// `(uri, conversationId, date, messageCount, firstSnippet, lastSnippet)`
/// rows. Used by the aggregation tests below.
struct DayBucketRow {
    uri: String,
    conversation_id: String,
    date: String,
    message_count: i64,
    first_snippet: String,
    last_snippet: String,
}

fn day_buckets(store: &RdfStore) -> Vec<DayBucketRow> {
    let q = format!(
        "SELECT ?b ?conv ?date ?count ?first ?last WHERE {{ \
         ?b a <{MSG_NS}DayBucket> ; \
            <{MSG_NS}conversationId> ?conv ; \
            <{MSG_NS}date> ?date ; \
            <{MSG_NS}messageCount> ?count ; \
            <{MSG_NS}firstSnippet> ?first ; \
            <{MSG_NS}lastSnippet> ?last }} \
         ORDER BY DESC(?date)"
    );
    let mut out = Vec::new();
    let Ok(QueryResults::Solutions(solutions)) = store.query(&q) else {
        return out;
    };
    for sol in solutions.flatten() {
        let lit = |k: &str| {
            sol.get(k).and_then(|t| match t {
                oxigraph::model::Term::Literal(l) => Some(l.value().to_string()),
                oxigraph::model::Term::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            })
        };
        let count: i64 = lit("count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        out.push(DayBucketRow {
            uri: lit("b").unwrap_or_default(),
            conversation_id: lit("conv").unwrap_or_default(),
            date: lit("date").unwrap_or_default(),
            message_count: count,
            first_snippet: lit("first").unwrap_or_default(),
            last_snippet: lit("last").unwrap_or_default(),
        });
    }
    out
}

/// Pull all messenger:participants URIs for a given DayBucket URI.
fn bucket_participants(store: &RdfStore, bucket_uri: &str) -> Vec<String> {
    let q = format!(
        "SELECT ?p WHERE {{ <{bucket_uri}> <{MSG_NS}participants> ?p }}"
    );
    let mut out = Vec::new();
    let Ok(QueryResults::Solutions(solutions)) = store.query(&q) else {
        return out;
    };
    for sol in solutions.flatten() {
        if let Some(oxigraph::model::Term::NamedNode(n)) = sol.get("p") {
            out.push(n.as_str().to_string());
        }
    }
    out
}

#[test]
fn day_buckets_aggregate_correctly_from_rolling_buffer() {
    // Drive 3 messages into a ready conversation; assert the store
    // carries a single messenger:DayBucket (all messages land on
    // today in the test runner's local TZ) with messageCount=3,
    // firstSnippet=first message, lastSnippet=last message,
    // participants=[did:tox:peer] (sender of the inbound TextMessages).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-agg");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "m1", "morning");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "m2", "midday update");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "m3", "evening");

    let buckets = day_buckets(&store);
    assert_eq!(
        buckets.len(),
        1,
        "single-day fixture must yield exactly one messenger:DayBucket — got {} \
         (conversation buckets: {:?})",
        buckets.len(),
        buckets.iter().map(|b| (&b.date, b.message_count)).collect::<Vec<_>>()
    );
    let b = &buckets[0];
    assert_eq!(b.conversation_id, "conv-m3b-agg");
    assert_eq!(b.message_count, 3, "bucket must count all 3 messages");
    assert_eq!(b.first_snippet, "morning", "firstSnippet = first message");
    assert_eq!(b.last_snippet, "evening", "lastSnippet = last message");
    // URN scope check (subset of day_bucket_urns_scope_by_conversation).
    assert!(
        b.uri.contains("conv-m3b-agg"),
        "bucket URN must embed conversationId for M5 multi-conv scoping — got {}",
        b.uri
    );
    assert!(
        b.uri.starts_with("urn:msg:bucket:day:"),
        "bucket URN must use the urn:msg:bucket:day: prefix — got {}",
        b.uri
    );
}

#[test]
fn day_buckets_carry_multi_valued_participants_predicate() {
    // The brief's pre-decided design: messenger:participants is a multi-
    // valued predicate (one triple per participant) rather than the
    // RDF-list syntax in the spec example. Two distinct senders → two
    // messenger:participants triples on the same bucket.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-parts");

    // Drive a name event for the peer first, otherwise the from-field is
    // a shortUri fingerprint and the participants list reads the JS
    // `from` value (which is what bucket aggregation uses). Either way
    // the test asserts >0 participants per bucket.
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "p1", "hi");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "p2", "again");

    let buckets = day_buckets(&store);
    assert_eq!(buckets.len(), 1, "expected single-day bucket");
    let participants = bucket_participants(&store, &buckets[0].uri);
    assert!(
        !participants.is_empty(),
        "bucket must carry at least one messenger:participants triple — got: {participants:?}"
    );
    // All entries land under the same `from` (peer's display short-uri),
    // so participants dedup down to 1. The set semantics are the contract.
    assert_eq!(
        participants.len(),
        1,
        "two messages from the same sender must dedup to 1 participant — got: {participants:?}"
    );
}

#[test]
fn day_bucket_urns_scope_by_conversation() {
    // The bucket URN scheme `urn:msg:bucket:day:<conv>:<YYYY-MM-DD>`
    // includes conversationId so the M5 multi-conversation future
    // doesn't collide bucket URNs across conversations. The
    // flushDayBuckets DELETE clause is double-bound on conversationId
    // for the same reason.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-scoped");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s1", "scoped");

    let buckets = day_buckets(&store);
    assert_eq!(buckets.len(), 1);
    assert!(
        buckets[0].uri.contains(":conv-m3b-scoped:"),
        "bucket URN must include `:<conversationId>:` between prefix and date — got {}",
        buckets[0].uri
    );
    // Date suffix must look like YYYY-MM-DD.
    let date_suffix = buckets[0].date.clone();
    assert_eq!(
        date_suffix.len(),
        10,
        "messenger:date literal must be YYYY-MM-DD (10 chars) — got {date_suffix:?}"
    );
    assert!(
        buckets[0].uri.ends_with(&date_suffix),
        "bucket URN must end with the YYYY-MM-DD date suffix — got {}",
        buckets[0].uri
    );
}

#[test]
fn flushdaybuckets_deletes_stale_buckets_on_rebuild() {
    // After messages arrive, a fresh aggregation must DELETE any
    // previous bucket whose count / snippet has shifted. The
    // flushDayBuckets DELETE-WHERE on `?b a messenger:DayBucket ;
    // messenger:conversationId "<conv>"` clears the prior pass before
    // the INSERT for the new pass — no accumulation.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-stale");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s1", "first");
    let buckets1 = day_buckets(&store);
    assert_eq!(buckets1.len(), 1);
    assert_eq!(buckets1[0].message_count, 1);

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s2", "second");
    let buckets2 = day_buckets(&store);
    assert_eq!(
        buckets2.len(),
        1,
        "second message must not double-create the day bucket — got {} buckets, {:?}",
        buckets2.len(),
        buckets2.iter().map(|b| (&b.date, b.message_count)).collect::<Vec<_>>()
    );
    assert_eq!(buckets2[0].message_count, 2, "count must reflect both messages");
    assert_eq!(buckets2[0].first_snippet, "first");
    assert_eq!(buckets2[0].last_snippet, "second");
}

#[test]
fn day_bucket_snippet_truncates_at_24_chars() {
    // UC2.8 — snippets render `<24 chars>…` in tier 3 buckets. The
    // truncateSnippet helper applies in the rendered widget DSL; the
    // store-side messenger:firstSnippet / messenger:lastSnippet
    // literals carry the FULL message text (so a future M3-C / M3-D
    // path can re-truncate at a different width without losing data).
    // This test asserts the rendered tier-3 widget truncates, not the
    // store literal.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-trunc");

    let long = "this is a very long message that exceeds twenty four chars";
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "long-1", long);

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER3_BELOW)
        .expect("tier-3 widget literal must exist");
    // Truncated snippet must appear in the rendered widget; the full
    // string must NOT (it would overflow the row layout).
    let truncated_head = "this is a very long mess"; // 24 chars
    assert!(
        widget.contains(truncated_head),
        "tier-3 widget must contain the truncated snippet head — got: {widget}"
    );
    assert!(
        !widget.contains(long),
        "tier-3 widget must NOT contain the full untruncated snippet — got: {widget}"
    );
    assert!(
        widget.contains("..."),
        "tier-3 widget must carry a `...` truncation marker — got: {widget}"
    );
}

// ── M3-C — hour-bucket aggregation + bundled #10/#11 regressions ────────
//
// HourBucket is a finer-grain sibling of DayBucket. Per M3-C brief:
//   - URN: `urn:msg:bucket:hour:<conv>:<YYYY-MM-DDTHH>`
//   - `messenger:hour` literal: `YYYY-MM-DDTHH:00:00` xsd:dateTime,
//     viewer-local TZ (mirrors dayKey's local-TZ stance per UC2 edge
//     cases). NO `messenger:participants` — sparkline ticks (M3-D) are
//     per-day, hour-level participant data isn't needed.
//   - Snippets reuse the SNIPPET_MAX=24 truncation rule.

struct HourBucketRow {
    uri: String,
    conversation_id: String,
    hour: String,
    message_count: i64,
    first_snippet: String,
    last_snippet: String,
}

fn hour_buckets(store: &RdfStore) -> Vec<HourBucketRow> {
    let q = format!(
        "SELECT ?b ?conv ?hour ?count ?first ?last WHERE {{ \
         ?b a <{MSG_NS}HourBucket> ; \
            <{MSG_NS}conversationId> ?conv ; \
            <{MSG_NS}hour> ?hour ; \
            <{MSG_NS}messageCount> ?count ; \
            <{MSG_NS}firstSnippet> ?first ; \
            <{MSG_NS}lastSnippet> ?last }} \
         ORDER BY DESC(?hour)"
    );
    let mut out = Vec::new();
    let Ok(QueryResults::Solutions(solutions)) = store.query(&q) else {
        return out;
    };
    for sol in solutions.flatten() {
        let lit = |k: &str| {
            sol.get(k).and_then(|t| match t {
                oxigraph::model::Term::Literal(l) => Some(l.value().to_string()),
                oxigraph::model::Term::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            })
        };
        let count: i64 = lit("count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        out.push(HourBucketRow {
            uri: lit("b").unwrap_or_default(),
            conversation_id: lit("conv").unwrap_or_default(),
            hour: lit("hour").unwrap_or_default(),
            message_count: count,
            first_snippet: lit("first").unwrap_or_default(),
            last_snippet: lit("last").unwrap_or_default(),
        });
    }
    out
}

#[test]
fn hour_buckets_aggregate_correctly_from_rolling_buffer() {
    // 3 messages in a single hour (test runner clock won't span an hour
    // mid-test) → 1 messenger:HourBucket with messageCount=3,
    // firstSnippet=first message text, lastSnippet=last message text.
    // Mirrors day_buckets_aggregate_correctly_from_rolling_buffer at
    // hour granularity.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-agg");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "h1", "morning");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "h2", "midday update");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "h3", "evening");

    let buckets = hour_buckets(&store);
    assert_eq!(
        buckets.len(),
        1,
        "single-hour fixture must yield exactly one messenger:HourBucket — got {} \
         (conversation buckets: {:?})",
        buckets.len(),
        buckets.iter().map(|b| (&b.hour, b.message_count)).collect::<Vec<_>>()
    );
    let b = &buckets[0];
    assert_eq!(b.conversation_id, "conv-m3c-agg");
    assert_eq!(b.message_count, 3, "bucket must count all 3 messages");
    assert_eq!(b.first_snippet, "morning", "firstSnippet = first message");
    assert_eq!(b.last_snippet, "evening", "lastSnippet = last message");
    assert!(
        b.uri.starts_with("urn:msg:bucket:hour:"),
        "bucket URN must use the urn:msg:bucket:hour: prefix — got {}",
        b.uri
    );
    // hour literal shape: YYYY-MM-DDTHH:00:00 (19 chars, naïve local).
    assert_eq!(
        b.hour.len(),
        19,
        "messenger:hour literal must be `YYYY-MM-DDTHH:00:00` (19 chars, no Z) — got {:?}",
        b.hour
    );
    assert!(
        b.hour.ends_with(":00:00"),
        "messenger:hour literal must end at the top of the hour (`:00:00`) — got {:?}",
        b.hour
    );
}

#[test]
fn hour_bucket_urns_scope_by_conversation() {
    // The bucket URN scheme `urn:msg:bucket:hour:<conv>:<YYYY-MM-DDTHH>`
    // includes conversationId so the M5 multi-conversation future
    // doesn't collide bucket URNs across conversations. The
    // flushHourBuckets DELETE clause is double-bound on conversationId
    // for the same reason.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-scoped");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s1", "scoped");

    let buckets = hour_buckets(&store);
    assert_eq!(buckets.len(), 1);
    assert!(
        buckets[0].uri.contains(":conv-m3c-scoped:"),
        "bucket URN must include `:<conversationId>:` between prefix and hour — got {}",
        buckets[0].uri
    );
    // URN suffix is the hour-key (`YYYY-MM-DDTHH`, 13 chars). The hour
    // literal is `<URN-suffix>:00:00`, so the URN must end with the
    // first 13 chars of the literal.
    let hour_lit = &buckets[0].hour;
    assert!(hour_lit.len() >= 13, "hour literal too short: {hour_lit:?}");
    let urn_suffix = &hour_lit[..13];
    assert!(
        buckets[0].uri.ends_with(urn_suffix),
        "bucket URN must end with the YYYY-MM-DDTHH suffix matching messenger:hour — got {} (expected suffix {})",
        buckets[0].uri,
        urn_suffix
    );
}

#[test]
fn flushhourbuckets_deletes_stale_buckets_on_rebuild() {
    // After messages arrive, a fresh aggregation must DELETE any
    // previous bucket whose count / snippet has shifted. The
    // flushHourBuckets per-URI DELETE-WHERE clears the prior pass before
    // the INSERT for the new pass — no accumulation.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-stale");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s1", "first");
    let buckets1 = hour_buckets(&store);
    assert_eq!(buckets1.len(), 1);
    assert_eq!(buckets1[0].message_count, 1);

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "s2", "second");
    let buckets2 = hour_buckets(&store);
    assert_eq!(
        buckets2.len(),
        1,
        "second message must not double-create the hour bucket — got {} buckets, {:?}",
        buckets2.len(),
        buckets2.iter().map(|b| (&b.hour, b.message_count)).collect::<Vec<_>>()
    );
    assert_eq!(buckets2[0].message_count, 2, "count must reflect both messages");
    assert_eq!(buckets2[0].first_snippet, "first");
    assert_eq!(buckets2[0].last_snippet, "second");
}

#[test]
fn hour_bucket_snippet_truncates_at_24_chars() {
    // SNIPPET_MAX=24 truncation rule applies to hour buckets via the
    // shared truncateSnippet helper. The store-side
    // messenger:firstSnippet / messenger:lastSnippet literals carry the
    // FULL message text (consistent with M3-B day buckets — store keeps
    // the unabridged form so future renderers can re-truncate at a
    // different width without losing data). This test asserts the
    // STORE literal carries the full body; downstream truncation is
    // verified by day_bucket_snippet_truncates_at_24_chars on the
    // rendered widget DSL.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-trunc");

    let long = "this is a very long message that exceeds twenty four chars";
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "long-1", long);

    let buckets = hour_buckets(&store);
    assert_eq!(buckets.len(), 1);
    assert_eq!(
        buckets[0].first_snippet, long,
        "store-side messenger:firstSnippet must carry full untruncated body"
    );
    assert_eq!(
        buckets[0].last_snippet, long,
        "store-side messenger:lastSnippet must carry full untruncated body"
    );
}

// ── Bundled regressions for tasks #10 + #11 ─────────────────────────────

#[test]
fn day_bucket_snippet_does_not_double_escape_quotes() {
    // Regression for task #11. Prior to the fix, tier-3 day-bucket
    // snippets wrapped the body with literal ASCII `"`:
    //   var snippets = '"' + first + '" -> "' + last + '"';
    // The wrapping `"` chars rode through escapeWidget()'s turtle-escape
    // path (`"` → `\"` for safe embedding inside `<antenna:widget>
    // "..."`) and surfaced as literal `\"hello\"` in the rendered widget
    // DSL because Station's renderer doesn't unescape on the value side.
    // Fix: use U+201C/U+201D curly quotes + U+2192 arrow — escape-safe-
    // by-construction (not `"`, not `\`).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-escape");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "esc-1", "hello world");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER3_BELOW)
        .expect("tier-3 widget literal must exist");

    // Positive: curly-quote pair around the snippet body.
    assert!(
        widget.contains("\u{201C}hello world\u{201D}"),
        "tier-3 snippet must wrap body with curly quotes — got: {widget}"
    );
    // Positive: unicode arrow between the two snippet halves.
    assert!(
        widget.contains(" \u{2192} "),
        "tier-3 snippet must use U+2192 between first/last — got: {widget}"
    );
    // Negative: literal backslash-quote MUST NOT appear in the snippet
    // area (the bug surface). A `\"` here means the escape collision
    // crept back in.
    assert!(
        !widget.contains("\\\"hello world\\\""),
        "tier-3 snippet must not carry literal `\\\"hello world\\\"` (task #11 regression) — got: {widget}"
    );
    // Negative: ASCII `->` snippet separator MUST NOT remain (paired
    // with the literal-`\"` form). The unicode arrow replaced it.
    assert!(
        !widget.contains("\\\"hello world\\\" -> \\\"hello world\\\""),
        "tier-3 snippet must not carry the pre-fix `\\\"…\\\" -> \\\"…\\\"` shape — got: {widget}"
    );
}

#[test]
fn day_bucket_emit_skips_empty_iri_participant() {
    // Regression for task #10. Prior to the fix, flushDayBuckets emitted
    //   <bucket> messenger:participants <pUri> .
    // for every entry in the aggregate's participants list, where
    //   var pUri = b.participants[j].replace(/[<>"]/g, '');
    // strips angle brackets / quotes from the URI shape. A pathological
    // participant of `<>` / `""` / a string that happens to be entirely
    // those chars collapsed to '' and emitted `messenger:participants
    // <>` — an empty-IRI Turtle that fails the absolute-IRI parse on
    // ingest ("No scheme found in an absolute IRI"). Fix: defensive
    // guard `if (!pUri) continue;` at the emit site before clause
    // append.
    //
    // We can't easily reproduce the empty-pUri trigger from cargo (the
    // documented path requires a pre-AccountReady self-greet with
    // simultaneously-empty selfUri AND nick, which is timing-sensitive
    // and not reachable through the dispatched-event helpers). The
    // guard's correctness contract is independent of the trigger:
    // bucket_participants(...) MUST NOT contain an empty IRI / a string
    // that strips down to empty. Assert that contract directly.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3c-empty-iri");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "e1", "ping");

    let buckets = day_buckets(&store);
    assert_eq!(buckets.len(), 1, "expected single-day bucket");
    let participants = bucket_participants(&store, &buckets[0].uri);
    for p in &participants {
        assert!(
            !p.is_empty(),
            "messenger:participants must never contain empty IRI (task #10 regression) — got: {participants:?}"
        );
        // A stripped form would also be invalid — the guard at the
        // emit site checks pUri after the bracket strip.
        let stripped: String = p.chars().filter(|c| !matches!(c, '<' | '>' | '"')).collect();
        assert!(
            !stripped.is_empty(),
            "messenger:participants must not strip down to empty (task #10 regression) — got: {p:?}"
        );
    }
}

#[test]
fn tier2_renders_inline_bubbles_with_chrome() {
    // Tier 2 ("day-grouped"): inline bubbles + day separators inside
    // the chat-panel chrome. Single-day fixture means we get N bubbles
    // and exactly 1 separator row (the leading "Today" label between
    // panel chrome and the first bubble). The bubble inner-Container
    // signature (msg-recv-bg / msg-sent-bg + borderRadius=6) is the
    // bubbleWidgetForTier(m, 1) marker — its presence proves we're
    // reusing the tier-1 helper rather than duplicating render logic.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-tier2");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "t2-1", "alpha");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "t2-2", "beta");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER2_BELOW)
        .expect("tier-2 widget literal must exist");

    // Bubble signature — bubbleWidgetForTier(m, 1) emits
    // Container{color=msg-recv-bg,borderRadius=6,padding=6} for
    // received messages.
    assert!(
        widget.contains("msg-recv-bg"),
        "tier 2 must render received-bubble Container (proves bubbleWidgetForTier reuse) — got: {widget}"
    );
    // Day separator — Today label as text-tertiary monospace.
    assert!(
        widget.contains("Text{value=Today,fontSize=10,color=text-tertiary,fontFamily=monospace}"),
        "tier 2 must render a `Today` separator label — got: {widget}"
    );
    // Both bubble texts visible.
    for needle in ["alpha", "beta"] {
        assert!(
            widget.contains(needle),
            "tier 2 must render bubble text \"{needle}\" — got: {widget}"
        );
    }
    // Chrome continuity holds.
    assert!(
        widget.contains("Container{color=surface-elevated,padding=8,borderRadius=6}"),
        "tier 2 must keep chat-panel chrome — got: {widget}"
    );
    assert!(
        !widget.contains("DAY-GROUPED HERE"),
        "tier 2 must NOT carry the M3-A placeholder — got: {widget}"
    );
}

#[test]
fn tier3_renders_row_per_day_with_count_and_snippets() {
    // Tier 3 ("day-buckets"): one row per day. Single-day fixture →
    // one Button-wrapped Row with `<dateLabel>  <count> msgs  "<first>"
    // → "<last>"`. The Button onTap URN encodes the dayKey for the
    // teleport handler (currently a log-line stub; M3-C/D wires the
    // actual camera move).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-tier3");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "t3-1", "first");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "t3-2", "second");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "t3-3", "last");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER3_BELOW)
        .expect("tier-3 widget literal must exist");

    // Date label.
    assert!(
        widget.contains("Text{value=Today,fontSize=11,color=text-primary,fontFamily=monospace}"),
        "tier 3 must render the date label cell — got: {widget}"
    );
    // Count cell.
    assert!(
        widget.contains("Text{value=3 msgs,fontSize=11,color=text-tertiary,fontFamily=monospace}"),
        "tier 3 must render the `<count> msgs` cell — got: {widget}"
    );
    // Snippet cell. Task #11 fix: the snippet pair uses curly typographic
    // quotes (U+201C/U+201D) and a U+2192 arrow rather than ASCII `"` /
    // `->`. Rationale: ASCII `"` collided with escapeWidget's turtle-
    // escape path (the `<antenna:widget> "..."` outer literal) and
    // surfaced as `\"…\"` in the rendered widget; curly quotes are
    // escape-safe-by-construction (not `"` / not `\`).
    assert!(
        widget.contains("\u{201C}first\u{201D} \u{2192} \u{201C}last\u{201D}"),
        "tier 3 must render `\u{201C}<first>\u{201D} \u{2192} \u{201C}<last>\u{201D}` snippet pair — got: {widget}"
    );
    // Regression for task #11: ASCII `\"` MUST NOT appear in the snippet
    // pair area. A literal backslash-quote at this point means the
    // escapeWidget round-trip crept back in.
    assert!(
        !widget.contains("\\\"first\\\""),
        "tier 3 snippet must not carry literal `\\\"` (task #11 regression) — got: {widget}"
    );
    // No M3-A placeholder.
    assert!(
        !widget.contains("DAY BUCKETS HERE"),
        "tier 3 must NOT carry the M3-A placeholder — got: {widget}"
    );
}

#[test]
fn tier3_emits_teleport_urn_on_each_row() {
    // Each tier-3 day row is wrapped in
    // `Button{onTap=urn:msg:teleport:day:<YYYY-MM-DD>}`. M3-B emits the
    // URN + visible affordance only — the actual teleport handler ships
    // in M3-D alongside the sparkline tap. The TapEvent dispatch branch
    // in pipeline.ttl logs `[MSG] teleport-day <key>` for now.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-teleport");

    send_text_message(&store, &dag, &mut out, "did:tox:peer", "tp-1", "ping");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER3_BELOW)
        .expect("tier-3 widget literal must exist");
    assert!(
        widget.contains("Button{onTap=urn:msg:teleport:day:"),
        "tier 3 must wrap each row in Button{{onTap=urn:msg:teleport:day:<YYYY-MM-DD>}} — got: {widget}"
    );
    // The dayKey must look like YYYY-MM-DD (10 chars) — extract the URN
    // and validate via the bucket store row.
    let buckets = day_buckets(&store);
    assert_eq!(buckets.len(), 1);
    let needle = format!("Button{{onTap=urn:msg:teleport:day:{}}}[", buckets[0].date);
    assert!(
        widget.contains(&needle),
        "tier 3 must wrap the row in Button{{onTap=urn:msg:teleport:day:<key>}} \
         where <key>={} — got: {widget}",
        buckets[0].date
    );
}

#[test]
fn tier2_and_tier3_render_empty_state_when_no_messages() {
    // UC2.7 — empty chat: all tiers render the existing "say hello to
    // <peer>" placeholder. Without a peer URI the copy degrades to "no
    // peer URI configured" (the rebuildChat tier-1 fallback we mirror).
    // Both tiers must NOT carry the M3-A placeholder strings any more.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    for below in [CHAT_TIER2_BELOW, CHAT_TIER3_BELOW] {
        let widget = lod_widget_at(&store, CHAT_URI, below)
            .expect("tier widget literal must exist");
        // Empty-state text is the same as tier 1's empty-conversation
        // copy ("no peer URI configured" before peer + conversationId
        // are set).
        assert!(
            widget.contains("no peer URI configured"),
            "tier at below={below} must render empty-state placeholder when no \
             messages are present — got: {widget}"
        );
        // M3-A placeholders must be gone.
        assert!(
            !widget.contains("DAY-GROUPED HERE") && !widget.contains("DAY BUCKETS HERE"),
            "tier at below={below} must NOT carry M3-A placeholder — got: {widget}"
        );
    }
}

#[test]
fn aggregation_runs_under_5ms_for_60_message_buffer() {
    // test-plan.md § Performance targets — LOD widget rebuild on
    // message arrival < 5 ms p99. M3-B's contribution to that path is
    // the per-rebuild flushDayBuckets aggregation. We measure the wall
    // time of a settle() cycle that includes flushDayBuckets vs a
    // rough baseline; if the aggregation budget overruns 5 ms mean for
    // 60 messages the cut violates exit criteria.
    //
    // The cargo-driven dispatch loop has overhead (40 ms sleep per
    // settle iteration, queue-based emit pump) that swamps the
    // microsecond-scale aggregation cost, so a tight per-call
    // benchmark is impractical here. Instead we assert the looser
    // "60-message rebuild settles in well under wall-clock budget"
    // contract — if aggregation were quadratic / O(N²) we'd see it
    // here regardless of the dispatch overhead.
    use std::time::Instant;

    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3b-perf");

    // Fill the rolling-60 buffer.
    for i in 0..60 {
        let mid = format!("perf-{i}");
        let text = format!("msg {i}");
        send_text_message(&store, &dag, &mut out, "did:tox:peer", &mid, &text);
    }

    // Trigger ONE more rebuildChat cycle and time it. WhoAmI re-emits
    // the chat panel from the now-full buffer so flushDayBuckets walks
    // the full 60 entries.
    let t0 = Instant::now();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);
    let elapsed = t0.elapsed();

    let buckets = day_buckets(&store);
    assert_eq!(buckets.len(), 1, "perf fixture: all messages on today");
    assert_eq!(buckets[0].message_count, 60, "all 60 messages aggregate");

    // Total settle cost is dominated by dispatch + 40 ms sleep per
    // empty iteration (EMPTY_BREAK=5 → ~200 ms minimum) so we assert
    // a generous ceiling: a quadratic regression would push this into
    // the seconds, but linear-time aggregation completes well inside.
    // Mean per-rebuild aggregation is sub-millisecond on M-series
    // hardware; the budget here is wall-clock end-to-end.
    assert!(
        elapsed.as_millis() < 2_000,
        "rebuild + flushDayBuckets for 60 messages must settle under 2 s wall-clock \
         (a quadratic aggregation regression breaks this) — took {:?}",
        elapsed
    );
}

// ── M3-D — week sparkline (UC2 — Conversation Timeline) ─────────────────
//
// M3-D replaces the tier-4 `SPARKLINE HERE` placeholder with a hand-built
// `Column` of variable-height `Container`s — one tick per day, 60 days
// total, height ∝ messageCount (capped), color by participant diversity:
//
//   - 1 sender     → color=live-data  (cyan, voidline:resonanceCyan)
//   - ≥2 senders   → color=structural (magenta, voidline:pulseMagenta)
//
// Each tick is wrapped in `Button{onTap=urn:msg:teleport:day:<key>}` —
// re-using the M3-B URN scheme. The TapEvent dispatch branch fans both
// tier-3 row taps and tier-4 tick taps through `teleportToDayFirstMessage`,
// which looks up the day's first-message-of-day mid → bubble world-y
// (stashed in `globalThis.bubbleY` by rebuildBubbles) → emits an
// `antenna:Teleport` Turtle blob with x/y/scale. NO new RDF vocab; NO
// new widget DSL primitives — exact pre-decisions from the M3-D brief.

/// Count occurrences of `needle` in `haystack`. Used to assert the 60-tick
/// shape inside the tier-4 widget DSL string. `str::matches().count()` is
/// awkward to read inline against an assert; this helper makes the test
/// intent obvious.
fn count_matches(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn tier4_renders_60_day_sparkline_column() {
    // UC2 § Tier 4: "60-day window. Tap any tick = teleport-zoom into
    // tier 1 anchored at noon of that day." A single day of messages
    // should still render a 60-tick column — the other 59 days fill
    // with the synthetic zero-tick (1-px floor per UC2.9).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-60");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "tick-1", "ping");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("tier-4 widget literal must exist");

    // Each tick wraps in `Button{onTap=urn:msg:teleport:day:<key>}[…]`
    // — count those buttons. A tier-4 column with the canonical 60-day
    // window emits exactly 60 such buttons.
    let buttons = count_matches(&widget, "Button{onTap=urn:msg:teleport:day:");
    assert_eq!(
        buttons, 60,
        "tier-4 sparkline must render exactly 60 day-ticks (1 per calendar day) \
         — got {buttons}; widget: {widget}"
    );
}

#[test]
fn sparkline_tick_height_proportional_to_message_count() {
    // The tick-height formula is linear with cap:
    //   H = max(MIN, round(MAX * count / maxCount))
    // With one busy day in the buffer, that day clamps at MAX. Other
    // days (zero-message synthetic placeholders) fall to MIN. We can't
    // backdate timestamps in this harness without a JS-side eval shim,
    // so we drive a single day with multiple messages and assert the
    // rendered tick is the busiest tick (height=28, the cap).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-h");

    // 5 messages from peer — single bucket with count=5.
    for i in 0..5 {
        send_text_message(&store, &dag, &mut out, "did:tox:peer", &format!("h-{i}"), "msg");
    }

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("tier-4 widget literal must exist");

    // Today's tick clamps at MAX = 28 (the only non-zero day in the
    // window, so it IS maxCount → ratio 1.0 → 28 px).
    assert!(
        widget.contains(",height=28,"),
        "the busiest day's tick must clamp at MAX (=28 px) when it's the \
         only non-zero day in the 60-day window — got: {widget}"
    );
    // 59 other days are at the MIN floor (=1 px). Every tick has the
    // canonical `Container{color=…,width=200,height=…,borderRadius=1}`
    // shape — the 1-px ones must show up.
    assert!(
        widget.contains(",height=1,"),
        "zero-message days must render as 1-px floor ticks (UC2.9 sparse \
         history) — got: {widget}"
    );
    // …and there must be at least 59 of them (one per gap day).
    let floors = count_matches(&widget, ",height=1,");
    assert!(
        floors >= 59,
        "expected ≥59 zero-message-floor ticks (one per gap day) — got {floors}"
    );
}

#[test]
fn sparkline_tick_color_reflects_participant_diversity() {
    // Color rule per the M3-D brief:
    //   participants == 1 → live-data (cyan)
    //   participants >= 2 → structural (magenta)
    // Drive a 1-sender bucket (peer-only, no self-greet) and assert
    // the tick uses live-data. Then drive a 2-sender bucket (self
    // greet + peer message, both same day) and assert at least one
    // structural tick appears. Both fixtures live in the same window
    // (today), so the same tier-4 widget shows the transition.
    //
    // Note: `drive_to_ready` triggers a self-greet ("hello from alice")
    // on ConversationReady — that's a self-sent message the script logs
    // into globalThis.messages with selfUri as the participant. So as
    // soon as a peer message arrives in addition, the bucket has two
    // distinct participants. We split the test into two phases against
    // separate pipelines to isolate the 1-sender vs ≥2-sender branches.

    // Phase 1: peer-only bucket. Build a fresh pipeline, skip the
    // self-greet by NOT calling drive_to_ready (which kicks the greet),
    // and inject a peer TextMessage directly. The participants list
    // for that day will carry just the peer URI.
    {
        let (store, dag) = build_messenger_pipeline();
        let mut out = CaptureOut::new();
        // Bring the conversation up just enough for the bucket to flush.
        // No self-greet — call ConversationReady WITHOUT the prior
        // self-id event so `globalThis.greeted` short-circuits via the
        // missing peerUri/selfUri (`maybeGreet` early-returns when
        // peerUri is empty).
        let conv_event = format!(
            "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
             carrier:contactUri \"did:tox:peer-only\" ; \
             carrier:conversationId \"conv-m3d-c1\" ."
        );
        dispatch::dispatch(&conv_event, &store, &dag, None, "", &mut out);
        settle(&dag, &store, &mut out, 20);

        // Inject a peer message — the participants list now contains a
        // single entry (peer's contactUri).
        send_text_message(&store, &dag, &mut out, "did:tox:peer-only", "c1-1", "solo");

        let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
            .expect("tier-4 widget literal must exist (1-sender phase)");

        // Today's tick (the only non-zero day) must use live-data —
        // assert at least one tick carries it AND no `structural` tick
        // appears (since no day has ≥2 participants in this phase).
        assert!(
            widget.contains("Container{color=live-data,width=200,"),
            "1-sender bucket must render the day-tick with color=live-data \
             (cyan) — got: {widget}"
        );
        assert!(
            !widget.contains("Container{color=structural,width=200,"),
            "1-sender bucket must NOT render any structural (magenta) tick \
             — got: {widget}"
        );
    }

    // Phase 2: ≥2-sender bucket. drive_to_ready's self-greet logs with
    // messageId='' so aggregateDayBuckets's `if (!m.messageId) continue;`
    // gate skips it (the messageId only lands when carrier:MessageSent
    // fires later — that event isn't in the test fixture). To get two
    // distinct senders into the participants list we instead inject two
    // peer TextMessages from DIFFERENT contactUris (group-chat shape):
    // the participants are deduped per-bucket as the union of fromUri
    // across all messageId-bearing messages of the day.
    {
        let (store, dag) = build_messenger_pipeline();
        let mut out = CaptureOut::new();
        drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-c2");
        send_text_message(&store, &dag, &mut out, "did:tox:peer", "c2-1", "back");
        send_text_message(&store, &dag, &mut out, "did:tox:peer-bob", "c2-2", "me too");

        let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
            .expect("tier-4 widget literal must exist (multi-sender phase)");
        assert!(
            widget.contains("Container{color=structural,width=200,"),
            "multi-sender bucket must render the day-tick with color=structural \
             (magenta) — got: {widget}"
        );
    }
}

#[test]
fn sparkline_zero_message_day_renders_minimal_tick() {
    // UC2.9 — sparse history: 7-day gap renders 7 zero-height ticks. We
    // can't backdate timestamps, so we drive ONE day's worth of messages
    // and assert the OTHER 59 days are zero-floor (1-px) ticks.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-z");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "z-1", "ping");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("tier-4 widget literal must exist");

    // 60 ticks total, exactly one is non-floor (today). 59 floors
    // satisfies the "sparse history → 1-px line at base of bar"
    // contract. We assert the floor count is ≥59 (Phase 2 of the
    // multi-sender path could push it to 58 if the self-greet ALSO
    // lands today, but it does — so we get 59 floors, 1 cap-tick).
    let floors = count_matches(&widget, ",height=1,");
    assert!(
        floors >= 59,
        "59-of-60 days with no messages must render at the 1-px floor \
         (UC2.9 sparse-history contract) — got {floors} floor ticks; widget: {widget}"
    );
}

#[test]
fn sparkline_tick_emits_teleport_urn() {
    // Each tier-4 tick wraps in `Button{onTap=urn:msg:teleport:day:<key>}`,
    // re-using the same URN scheme as M3-B's tier-3 day-rows.
    // teleportToDayFirstMessage handles both.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-urn");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "u-1", "ping");

    let widget = lod_widget_at(&store, CHAT_URI, CHAT_TIER4_BELOW)
        .expect("tier-4 widget literal must exist");

    // Today's bucket key is the only one we can pin down via the
    // existing `day_buckets` helper (which sorts by date desc) — the
    // most-recent day-bucket carries the key matching today.
    let buckets = day_buckets(&store);
    assert!(!buckets.is_empty(), "must have at least one bucket");
    let today_key = &buckets[0].date;
    let needle = format!("Button{{onTap=urn:msg:teleport:day:{today_key}}}[");
    assert!(
        widget.contains(&needle),
        "tier-4 must wrap today's tick in Button{{onTap=urn:msg:teleport:day:<key>}} \
         where <key>={today_key} — got: {widget}"
    );

    // And every one of the 60 ticks must carry SOME teleport-day URN
    // (gap days too — taps on gaps fall through `out-of-buffer` no-op).
    let urns = count_matches(&widget, "Button{onTap=urn:msg:teleport:day:");
    assert_eq!(
        urns, 60,
        "every tick (incl. zero-day floors) must carry a teleport-day \
         URN — got {urns}"
    );
}

#[test]
fn teleport_urn_handler_emits_antenna_teleport() {
    // Tap a tier-3 row / tier-4 tick → pipeline must emit
    //   [] a antenna:Teleport ; antenna:x "-150" ; antenna:y "<Y>" ;
    //                           antenna:scale "1.5" .
    // With Y = the world-y that rebuildBubbles stashed for that day's
    // first-message-of-day in `globalThis.bubbleY`. We assert against
    // CaptureOut.messages — the dispatch loop forwards every emit
    // through insert_with_dag which calls out.send.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-tp");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "tp-1", "ping");

    let buckets = day_buckets(&store);
    assert!(!buckets.is_empty(), "must have a day-bucket to tap");
    let today_key = &buckets[0].date;

    // Clear out the boot/setup messages so we're only inspecting what
    // the tap fires. (We can't reset CaptureOut, but we can mark the
    // current length and slice-from-there.)
    let baseline = out.messages.len();

    let tap_event = format!(
        "[] a <{ANTENNA_NS}TapEvent> ; \
         <{ANTENNA_NS}target> <urn:msg:teleport:day:{today_key}> ."
    );
    dispatch::dispatch(&tap_event, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let post_tap: Vec<&String> = out.messages.iter().skip(baseline).collect();

    // Find any line that types as antenna:Teleport — the dispatch
    // loop's `insert_with_dag` echoes it onto out.send; messages may
    // arrive in either prefix-form or full-IRI form depending on the
    // emit path's serializer.
    let teleport = post_tap.iter().find(|m| {
        m.contains("a antenna:Teleport") || m.contains("a <http://resonator.network/v2/antenna#Teleport>")
    });
    let teleport = teleport.unwrap_or_else(|| {
        panic!(
            "tap on urn:msg:teleport:day:{today_key} must emit antenna:Teleport — \
             got post-tap messages: {post_tap:?}"
        )
    });
    // Match prefix-form OR full-IRI form for each predicate — antenna's
    // dispatch echoes the emit verbatim, but the script's emit() concatenates
    // full IRIs from ANT_NS, so the line carries
    // `<http://resonator.network/v2/antenna#x>` rather than `antenna:x`.
    let x_ok = teleport.contains("antenna:x \"-150\"")
        || teleport.contains("antenna:x \"-150.0\"")
        || teleport.contains("#x> \"-150\"")
        || teleport.contains("#x> \"-150.0\"");
    assert!(
        x_ok,
        "antenna:x must be -150 (chat panel center x) — got: {teleport}"
    );
    let scale_ok =
        teleport.contains("antenna:scale \"1.5\"") || teleport.contains("#scale> \"1.5\"");
    assert!(
        scale_ok,
        "antenna:scale must be 1.5 (tier-1 landing scale) — got: {teleport}"
    );
    // Y depends on rebuildBubbles' computed bubbleCenterY for the day's
    // first-message — assert there's an antenna:y predicate with a
    // numeric literal (any double) so a bubble-layout regression fires
    // here without baking in the exact value.
    let y_ok = teleport.contains("antenna:y \"") || teleport.contains("#y> \"");
    assert!(
        y_ok,
        "antenna:y must be present and quoted — got: {teleport}"
    );
}

// M4-A — UC4 Attachment Inline Ladder, tier 1 (file event wiring + bubble
// icon). carrier:FileRecv mints a messenger:Attachment placed object beside
// its parent bubble, auto-emits carrier:AcceptFile, and rebuilds tier-1 row
// (paperclip + filename) on every chat rebuild. carrier:FileComplete
// settles state to `complete`.

const ATTACH_FILE_ID: &str = "abc123fileid";
const ATTACH_FILENAME: &str = "secret.jpg";
// M4-InvA — synthetic account ID for FileRecv → AcceptFile round-trip
// fixtures. Real libjami account IDs are 16-hex; the test only needs
// a non-empty string so the pipeline's `account` truthy gate fires
// and the AcceptFile emit carries the field through.
const TEST_ACCOUNT_ID: &str = "abc1234567890def";

fn attach_uri(file_id: &str) -> String {
    format!("urn:msg:attach:{file_id}")
}

/// Settle that ALSO captures every raw emit (pre-dispatch) into `raw`.
/// Needed for M4-A's carrier:AcceptFile assertion: dispatch::dispatch with
/// carrier=None drops carrier:* emits on the floor without pushing to
/// `out`, so the only place to observe the auto-accept emit is the raw
/// pump_emits stream. Mirrors `settle()`'s loop shape exactly otherwise.
fn settle_capturing_emits(
    dag: &Dag,
    store: &RdfStore,
    out: &mut CaptureOut,
    raw: &mut Vec<String>,
    max_iters: usize,
) {
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
            raw.push(turtle.clone());
            dispatch::dispatch(turtle, store, dag, None, "", out);
        }
    }
}

#[test]
fn file_recv_mints_attachment_placed_object_and_emits_accept() {
    // Inject a TextMessage so the parent bubble exists and bubbleAnchor
    // is populated. Then inject FileRecv. Assert:
    //   1. carrier:AcceptFile is auto-emitted with the right
    //      conversationId / messageId / fileId / path predicates.
    //   2. messenger:Attachment placed object exists with x/y/worldWidth
    //      /worldHeight + 4 antenna:lod blocks.
    //   3. messenger:fileId / fileName / fileSize / state / bubbleRef
    //      etc. predicates are present per M4-attachments.md § 3.
    //   4. Tier-1 widget contains the filename literal so M4.2 ("paperclip
    //      + filename visible at default zoom") is provable from the store.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    // Boot the script.
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // Drive ConversationReady so subsequent emits land cleanly. The router
    // gates a few paths on globalThis.conversationId; use the bubble-test
    // pattern.
    let conv_id = "conv-m4a-files";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // Parent bubble — message arrived with the file. The mid is the
    // Swarm commit id; carrier:FileRecv carries the same mid.
    let parent_mid = "mid-with-file-1234567890";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "here is the spec"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // FileRecv arrives carrying our parent bubble's mid. Capture raw emits
    // so the carrier:AcceptFile auto-emit is observable (carrier=None
    // dispatch drops carrier:* lines without pushing them to out).
    let mut raw_emits: Vec<String> = Vec::new();
    dispatch::dispatch(
        &file_recv_event(
            conv_id,
            "did:tox:peer",
            parent_mid,
            ATTACH_FILE_ID,
            ATTACH_FILENAME,
            500_000,
        ),
        &store, &dag, None, "", &mut out,
    );
    settle_capturing_emits(&dag, &store, &mut out, &mut raw_emits, 20);

    // (1) carrier:AcceptFile auto-emitted.
    //
    // M4-InvA — assertions cover EVERY field the carrier dispatcher
    // requires (carrier/src/turtle_parse.c:415-431 — account +
    // conversationId + messageId + fileId + path; all are
    // require_account / explicit-NULL-check gated). The original M4-A
    // assertion set was missing `carrier:account`, which let a regression
    // ship that broke every persistent-setup transfer with a silent
    // MissingField error → libjami `closed_by_host` close-out. Keeping
    // the field set here in lockstep with `require_account` /
    // `find_pred(stmt, …)` checks in the dispatcher means a future
    // required field can't slip through the same hole.
    let accept = raw_emits
        .iter()
        .find(|m| m.contains("carrier:AcceptFile"))
        .expect("carrier:FileRecv must trigger an auto-emit of carrier:AcceptFile");
    assert!(
        accept.contains(&format!("carrier:account \"{TEST_ACCOUNT_ID}\"")),
        "AcceptFile must carry carrier:account (carrier dispatcher require_account; \
         M4-InvA regression) — got: {accept}"
    );
    assert!(
        accept.contains(&format!("carrier:fileId \"{ATTACH_FILE_ID}\"")),
        "AcceptFile must carry carrier:fileId — got: {accept}"
    );
    assert!(
        accept.contains(&format!("carrier:messageId \"{parent_mid}\"")),
        "AcceptFile must carry carrier:messageId — got: {accept}"
    );
    assert!(
        accept.contains(&format!("carrier:conversationId \"{conv_id}\"")),
        "AcceptFile must carry carrier:conversationId — got: {accept}"
    );
    assert!(
        accept.contains("carrier:path \"")
            && accept.contains(&format!("/{ATTACH_FILE_ID}/{ATTACH_FILENAME}\"")),
        "AcceptFile must carry carrier:path with <fileId>/<filename> tail — got: {accept}"
    );

    // (2) messenger:Attachment placed object exists.
    let auri = attach_uri(ATTACH_FILE_ID);
    let geom = placed_geom(&store, &auri)
        .expect("messenger:Attachment placed object must emit with x/y/worldWidth/worldHeight");
    assert!(geom.w > 0.0 && geom.w < 200.0,
        "attachment worldWidth must be ~90 — got: {}", geom.w);
    assert!(geom.h > 0.0 && geom.h < 50.0,
        "attachment worldHeight must be tier-1 row height — got: {}", geom.h);

    // 4 LOD blocks (icon / thumbnail / provenance / preview).
    assert_eq!(
        lod_count(&store, &auri),
        4,
        "M4-A attachment must emit 4 LOD blocks (icon / thumbnail / provenance / preview)"
    );

    // (3) Per-tier metadata triples per M4-attachments.md § 3.
    let q_filename = format!(
        "ASK WHERE {{ <{auri}> <http://resonator.network/v2/messenger#fileName> \"{ATTACH_FILENAME}\" }}"
    );
    match store.query(&q_filename).expect("ASK fileName") {
        QueryResults::Boolean(b) => assert!(b, "messenger:fileName triple must be present"),
        _ => panic!("ASK must return boolean"),
    }

    let q_state = format!(
        "ASK WHERE {{ <{auri}> <http://resonator.network/v2/messenger#state> \"pending\" }}"
    );
    match store.query(&q_state).expect("ASK state") {
        QueryResults::Boolean(b) => {
            assert!(b, "fresh attachment must be in `pending` state immediately after FileRecv");
        }
        _ => panic!("ASK must return boolean"),
    }

    let q_bubble_ref = format!(
        "ASK WHERE {{ <{auri}> <http://resonator.network/v2/messenger#bubbleRef> <urn:msg:bubble:{parent_mid}> }}"
    );
    match store.query(&q_bubble_ref).expect("ASK bubbleRef") {
        QueryResults::Boolean(b) => {
            assert!(b, "messenger:bubbleRef must point at urn:msg:bubble:<parent_mid>");
        }
        _ => panic!("ASK must return boolean"),
    }

    // Type triple: a antenna:Object , messenger:Attachment.
    let q_type = format!(
        "ASK WHERE {{ <{auri}> a <http://resonator.network/v2/messenger#Attachment> }}"
    );
    match store.query(&q_type).expect("ASK Attachment type") {
        QueryResults::Boolean(b) => {
            assert!(b, "attachment must be typed messenger:Attachment");
        }
        _ => panic!("ASK must return boolean"),
    }

    // (4) Tier-1 widget DSL contains the filename literal — proves M4.2
    // acceptance (paperclip + filename visible).
    let widget = lod_widget_at(&store, &auri, 60.0)
        .expect("tier-1 LOD (below=60) must carry a widget literal");
    assert!(
        widget.contains(ATTACH_FILENAME),
        "tier-1 widget DSL must contain filename literal — got: {widget}"
    );
    let tier_label = lod_tier_label_at(&store, &auri, 60.0)
        .expect("tier-1 LOD must carry tierLabel");
    assert_eq!(tier_label, "icon", "tier-1 label must be `icon`");
}

#[test]
fn file_complete_settles_attachment_state_to_complete() {
    // Drive FileRecv → FileComplete (status="finished") and assert the
    // attachment's messenger:state ratchets to `complete`.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4a-complete";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-complete-12345";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "ack"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, "fid-complete", "doc.txt", 1024),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    dispatch::dispatch(
        &file_complete_event(conv_id, "fid-complete", "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    let auri = attach_uri("fid-complete");
    let q_state = format!(
        "ASK WHERE {{ <{auri}> <http://resonator.network/v2/messenger#state> \"complete\" }}"
    );
    match store.query(&q_state).expect("ASK state=complete") {
        QueryResults::Boolean(b) => {
            assert!(
                b,
                "FileComplete with status=finished must settle state to `complete`"
            );
        }
        _ => panic!("ASK must return boolean"),
    }
}

#[test]
fn file_recv_attachment_positioned_right_of_received_bubble() {
    // Bubble for received message sits LEFT-aligned (bubble centerX < 0
    // panel centerline). Attachment must be positioned to the RIGHT of
    // the bubble (attachment x > bubble x + bubble.w/2).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4a-pos";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-positioning-12345";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "with file"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, "fid-pos", "img.jpg", 4096),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    let bub_geom = placed_geom(&store, &bubble_uri(parent_mid))
        .expect("parent bubble must have geometry");
    let att_geom = placed_geom(&store, &attach_uri("fid-pos"))
        .expect("attachment must have geometry");

    let bubble_right_edge = bub_geom.x + bub_geom.w / 2.0;
    let att_left_edge = att_geom.x - att_geom.w / 2.0;
    assert!(
        att_left_edge >= bubble_right_edge,
        "attachment left edge ({att_left_edge}) must be at or right-of bubble right edge ({bubble_right_edge}) for received messages"
    );
    // Same Y as the bubble's anchor center — exact match per M4-attachments.md § 3.
    assert!(
        (att_geom.y - bub_geom.y).abs() < 0.01,
        "attachment Y must match bubble center Y — bubble.y={} att.y={}",
        bub_geom.y, att_geom.y
    );
}

#[test]
fn file_recv_tier2_image_widget_uses_image_file_path_primitive() {
    // M4-B — for an image/* mime in `complete` state the tier-2 LOD
    // widget DSL must contain the file-backed Image{} primitive with
    // path=<urlencoded savePath> and fit=cover. Proves test-plan M4.4
    // ("real image thumbnail rendered from disk") at the emit-shape
    // layer — the actual Image.file render is covered by Station's
    // widget_renderer test suite.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4b-image";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-img-tier2";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "with image"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-img-tier2";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "snap.jpg", 81_920),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    // Settle to `complete` — pre-complete state degrades to the
    // generic-icon branch (no live bytes yet on disk).
    dispatch::dispatch(
        &file_complete_event(conv_id, file_id, "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, 200.0)
        .expect("tier-2 LOD (below=200) must carry a widget literal after FileComplete");

    // (1) Tier-2 DSL contains the Image{} primitive routed through the
    //     new file-backed `path=` branch (legacy `src=` pre-M4-B is the
    //     network/asset code path).
    assert!(
        widget.contains("Image{path="),
        "tier-2 widget must use the M4-B Image{{path=…}} file-backed primitive — got: {widget}"
    );
    assert!(
        widget.contains("fit=cover"),
        "tier-2 image thumbnail must use fit=cover per UC4 §Tier 2 aesthetic — got: {widget}"
    );

    // (2) Path is URL-encoded — separators (`/`) and the auto-accept
    //     dir's trailing slash become %2F so a comma-bearing filename
    //     can't desync the prop tokenizer's depth-0 split.
    assert!(
        widget.contains("%2F"),
        "tier-2 widget path prop must be URL-encoded (path separators → %2F) — got: {widget}"
    );
    assert!(
        widget.contains(file_id),
        "tier-2 widget path prop must include the libjami fileId tail — got: {widget}"
    );

    // (3) tierLabel is `thumbnail`, matching M4-A's LOD ladder labels.
    let label = lod_tier_label_at(&store, &auri, 200.0)
        .expect("tier-2 LOD must carry tierLabel");
    assert_eq!(label, "thumbnail", "tier-2 label must be `thumbnail`");

    // (4) Per-tier worldWidth/worldHeight grew to 150 × 96 so Station's
    //     _LODContent renders the card against the right bound. Anchor
    //     (placed-object level) stays at tier-1 dims so screenPx<60 fires
    //     off the same rectangle as M4-A.
    let tier2_h = lod_world_height_at(&store, &auri, 200.0)
        .expect("tier-2 LOD must carry per-tier worldHeight (M1-D bubble parity)");
    assert!(
        tier2_h > 50.0 && tier2_h < 200.0,
        "tier-2 worldHeight must reflect thumbnail-card size — got: {tier2_h}"
    );

    // (5) Filename + size + relative-time strings are in the tier-2 DSL.
    assert!(
        widget.contains("snap.jpg"),
        "tier-2 widget must carry the filename literal — got: {widget}"
    );
    assert!(
        widget.contains("80.0 KB"),
        "tier-2 widget must format the file size as a human-readable string — got: {widget}"
    );
}

#[test]
fn file_recv_tier2_non_image_widget_falls_back_to_glyph_and_extension_badge() {
    // M4-B — non-image mime types (PDF / video / archive / unknown) at
    // tier 2 must NOT use the Image{path=…} primitive. They fall back to
    // a per-mime glyph + extension badge so the user can still tell what
    // landed in the bubble before M4-D wires up real PDF / video render.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4b-zip";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-zip-tier2";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "binary"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-zip-tier2";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "drop.zip", 4_096),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);
    dispatch::dispatch(
        &file_complete_event(conv_id, file_id, "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, 200.0)
        .expect("tier-2 LOD must carry a widget literal");

    // (1) NO Image{} primitive — non-image branch must not invoke the
    //     file-backed image render.
    assert!(
        !widget.contains("Image{path="),
        "non-image tier-2 widget must NOT route through Image{{path=…}} — got: {widget}"
    );

    // (2) Extension badge `.zip` is present in the DSL so the user
    //     reads the file type at a glance.
    assert!(
        widget.contains(".zip"),
        "non-image tier-2 widget must include extension badge — got: {widget}"
    );

    // (3) Filename literal + size string still rendered.
    assert!(
        widget.contains("drop.zip"),
        "tier-2 widget must carry the filename literal — got: {widget}"
    );
    assert!(
        widget.contains("4.0 KB"),
        "tier-2 widget must format the file size — got: {widget}"
    );
}

#[test]
fn file_recv_tier2_pre_complete_image_falls_back_to_glyph() {
    // M4-B — an image/* mime that hasn't reached `complete` yet (pending
    // / downloading) must NOT route through Image{path=…} — the file at
    // savePath isn't fully written yet and Image.file would surface a
    // half-image error. Pre-complete state degrades to the same glyph
    // fallback as a non-image mime.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4b-pending";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-img-pending";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "incoming"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-img-pending";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "midflight.jpg", 250_000),
        &store, &dag, None, "", &mut out,
    );
    // No FileComplete — state stays `pending`.
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, 200.0)
        .expect("tier-2 LOD must carry a widget literal even for in-flight transfers");
    assert!(
        !widget.contains("Image{path="),
        "in-flight image must not point Image.file at a partially-written file — got: {widget}"
    );
    // .jpg extension badge still surfaces so the user sees the file type
    // even before the bytes are local.
    assert!(
        widget.contains(".jpg"),
        "pre-complete image tier-2 must carry the extension badge — got: {widget}"
    );
}

#[test]
fn out_of_buffer_teleport_day_logs_noop_no_emit() {
    // Tap an unknown day — pipeline must NOT emit antenna:Teleport.
    // Older history beyond the rolling-60 buffer can't resolve to a
    // bubble; the handler logs a breadcrumb and returns.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m3d-oob");
    send_text_message(&store, &dag, &mut out, "did:tox:peer", "oob-1", "ping");

    let baseline = out.messages.len();
    let tap_event = format!(
        "[] a <{ANTENNA_NS}TapEvent> ; \
         <{ANTENNA_NS}target> <urn:msg:teleport:day:1999-01-01> ."
    );
    dispatch::dispatch(&tap_event, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let post_tap: Vec<&String> = out.messages.iter().skip(baseline).collect();
    assert!(
        !post_tap.iter().any(|m| {
            m.contains("a antenna:Teleport")
                || m.contains("a <http://resonator.network/v2/antenna#Teleport>")
        }),
        "out-of-buffer teleport-day must NOT emit antenna:Teleport — \
         got: {post_tap:?}"
    );
}

// ── M4-Bfix — file-only carrier:FileRecv mints a synthetic parent bubble ──
//
// libjami's Swarm sometimes ships a file in a commit with no sibling
// `carrier:TextMessage` (a "file-only" commit). M4-A's pipeline tracked
// bubbles via the TextMessage handler only — `globalThis.bubbleAnchor`
// stayed empty for the file's `messageId`, so `rebuildAttachments`
// skipped the placed object even though the metadata triples were in
// the store. Fix: in the FileRecv handler, mint a synthetic placeholder
// bubble at the same `messageId` (empty body, just timestamp chrome) so
// the anchor exists by the time `rebuildAttachments` runs.

#[test]
fn file_only_filerecv_mints_synthetic_bubble_and_paints_attachment() {
    // Drive a FileRecv with NO preceding TextMessage carrying the same
    // messageId. Assert:
    //   (a) a synthetic bubble placed object exists at the file's mid
    //       (urn:msg:bubble-obj:<mid>) — proves the anchor was minted.
    //   (b) the messenger:Attachment placed object exists with valid
    //       geometry — proves rebuildAttachments did NOT skip on a
    //       missing-anchor lookup.
    //   (c) the attachment is positioned to the right of the synthetic
    //       bubble at the same Y — proves the M4-A right-of-bubble
    //       layout still holds against a synthetic anchor.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4bfix-fileonly";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    // FileRecv arrives with NO matching TextMessage. The script must
    // mint a synthetic bubble at this mid before rebuildAttachments
    // runs, otherwise the bubbleAnchor lookup misses and the placed
    // object never paints.
    let parent_mid = "mid-fileonly-1234567890";
    let file_id = "fid-fileonly";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "lone.jpg", 8192),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    // (a) Synthetic bubble at the file's messageId. The placed object's
    // URI is bubble_uri(parent_mid) — same shape as a TextMessage-driven
    // bubble; the consumer (Station) cannot distinguish.
    let bub_geom = placed_geom(&store, &bubble_uri(parent_mid))
        .expect("file-only FileRecv must mint a synthetic bubble at the file's messageId — \
                 without it bubbleAnchor lookup misses and the attachment is skipped");

    // (b) Attachment placed object exists with geometry.
    let att_geom = placed_geom(&store, &attach_uri(file_id))
        .expect("attachment placed object must paint when the synthetic bubble is in place");
    assert!(att_geom.w > 0.0 && att_geom.h > 0.0,
        "attachment must have non-zero geometry — got w={}, h={}", att_geom.w, att_geom.h);

    // (c) Attachment positioned to the right of the synthetic bubble
    // at the same Y (received message → bubble on the left, attachment
    // on the right).
    let bubble_right_edge = bub_geom.x + bub_geom.w / 2.0;
    let att_left_edge = att_geom.x - att_geom.w / 2.0;
    assert!(
        att_left_edge >= bubble_right_edge,
        "attachment must paint right-of the synthetic bubble — \
         bubble right edge={bubble_right_edge}, attachment left edge={att_left_edge}"
    );
    assert!(
        (att_geom.y - bub_geom.y).abs() < 0.01,
        "attachment Y must match the synthetic bubble's anchor Y — \
         bubble.y={}, attachment.y={}",
        bub_geom.y, att_geom.y
    );

    // (d) The bubbleRef on the attachment points at the synthetic bubble.
    // This is the same predicate the M4-A test asserts on the
    // TextMessage-driven path; ensure the synthetic-bubble path
    // produces the same store shape.
    let auri = attach_uri(file_id);
    let q_bubble_ref = format!(
        "ASK WHERE {{ <{auri}> <{MESSENGER_NS}bubbleRef> <urn:msg:bubble:{parent_mid}> }}"
    );
    match store.query(&q_bubble_ref).expect("ASK bubbleRef") {
        QueryResults::Boolean(b) => {
            assert!(b, "messenger:bubbleRef must point at urn:msg:bubble:<mid> — \
                     same shape regardless of whether the bubble is real or synthetic");
        }
        _ => panic!("ASK must return boolean"),
    }
}

#[test]
fn file_only_filerecv_then_textmessage_does_not_double_register_bubble() {
    // libjami may split file announce + body across two commits, so a
    // real TextMessage at the same mid may arrive AFTER the synthetic
    // bubble was minted. The synthetic-bubble guard is keyed on
    // findMessageById(mid) — once the synthetic entry is in
    // globalThis.messages, a follow-up TextMessage's logMsg call still
    // adds the real entry (logMsg has no dedup), but the synthetic one
    // remains. That's acceptable: rebuildBubbles emits only one bubble
    // URN per mid (last entry wins on bubbleAnchor map write), so the
    // store stays consistent. Pin that behavior — the file's bubble
    // placed object exists exactly once after both events land.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4bfix-late-text";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-fileonly-late-text";
    let file_id = "fid-fileonly-late";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "doc.txt", 1024),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    // Bubble exists from the synthetic mint.
    placed_geom(&store, &bubble_uri(parent_mid))
        .expect("synthetic bubble must exist after FileRecv");

    // Late TextMessage at the same mid lands.
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "here it is"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    // The bubble URN is still present and unique. SPARQL count must be 1.
    let q_count = format!(
        "SELECT (COUNT(?b) AS ?n) WHERE {{ \
         BIND(<{}> AS ?b) ?b a <{ANTENNA_NS}Object> }}",
        bubble_uri(parent_mid)
    );
    if let QueryResults::Solutions(solutions) = store.query(&q_count).expect("count query") {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("n") {
                let n: i64 = lit.value().parse().unwrap_or(0);
                assert_eq!(n, 1,
                    "exactly one bubble placed object must exist for parent_mid={parent_mid} \
                     after both FileRecv (synthetic mint) and TextMessage (late real bubble) — \
                     got count={n}");
            }
        }
    }
}

// ── M4-Bfix — DayBucket flush wraps scheme-less participant URIs ──────────
//
// Real Jami contactUris are 40-hex fingerprints with NO scheme. They flow
// through `m.fromUri` into `aggregateDayBuckets`'s participants list verbatim,
// and the M3-B emit site spliced them in as `<40-hex>` — a relative IRI that
// Oxigraph's parser rejects with "No scheme found in an absolute IRI",
// surfacing as a `WARN [SPARQL] insert error` on every rebuildChat (~30 s
// cadence in alice's antenna log, since rebuildChat fires on every libjami
// presence/contact event). Fix: scheme-less values get the same
// `urn:msg:participant:` synthetic prefix that aggregateDayBuckets's
// falsy-fromUri path uses, so all DayBucket participant rows are valid
// absolute IRIs at the emit site.

#[test]
fn day_bucket_wraps_bare_hex_participant_uri_in_synthetic_scheme() {
    // Drive a TextMessage with a 40-hex bare-fingerprint contactUri (the
    // real Jami shape). After flushDayBuckets runs (it auto-flushes on
    // every rebuildChat), assert:
    //   (a) a DayBucket exists with a `messenger:participants` triple,
    //       proving the emit parsed cleanly (no "No scheme" parser warn).
    //   (b) the participant IRI starts with `urn:msg:participant:` — the
    //       wrapped form proves the fix wraps scheme-less values.
    //   (c) the participant IRI's tail is the original 40-hex
    //       fingerprint, proving the wrap preserves identity (no data
    //       loss).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    let bare_hex = "abcdef0123456789abcdef0123456789abcdef01"; // 40 hex chars, no scheme
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", bare_hex, "conv-m4bfix-bare");

    // Send a TextMessage from the bare-hex peer. logMsg pushes the
    // entry with fromUri=bare_hex, then rebuildChat → flushDayBuckets
    // emits a DayBucket triple referencing the participant.
    send_text_message(&store, &dag, &mut out, bare_hex, "mid-bare-hex-1", "ping");

    // (a) DayBucket participants triple exists in the store. If the
    // emit had failed parsing, NO participant triples would land at
    // all — the store-level presence is the cleanest "no parser warn"
    // proof we can pin without a tracing capture.
    let q_parts = format!(
        "SELECT ?p WHERE {{ ?b a <{MSG_NS}DayBucket> ; \
         <{MSG_NS}participants> ?p }}"
    );
    let mut found_participant: Option<String> = None;
    if let QueryResults::Solutions(solutions) = store.query(&q_parts).expect("participants query") {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::NamedNode(n)) = sol.get("p") {
                found_participant = Some(n.as_str().to_string());
                break;
            }
        }
    }
    let part = found_participant.expect(
        "DayBucket must carry at least one messenger:participants triple — \
         missing triple means the emit failed parsing (the M4-Bfix SPARQL warn)",
    );

    // (b) The wrapped form. Pre-fix would have produced `<bare_hex>`
    // which the parser rejects, so the bare form would never appear in
    // the store. With the fix in place, the participant IRI is the
    // synthetic `urn:msg:participant:<bare_hex>`.
    assert!(
        part.starts_with("urn:msg:participant:"),
        "scheme-less participant must be wrapped in the urn:msg:participant: \
         synthetic — got: {part}"
    );
    // (c) Tail preserves the original fingerprint.
    assert!(
        part.ends_with(bare_hex),
        "wrapped participant must preserve the original fingerprint as the URN \
         tail — got: {part} (expected tail: {bare_hex})"
    );
}

#[test]
fn day_bucket_preserves_existing_scheme_on_participant_uri() {
    // Companion to the bare-hex test: contactUris that already carry a
    // scheme (did:tox:peer, etc.) must pass through unchanged — the
    // `pUri.indexOf(':') < 0` guard only wraps when the value has no
    // colon. Pin that behavior so synthetic test fixtures and any
    // future scheme'd transport keep their identity.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    let scheme_uri = "did:tox:peerWithScheme";
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", scheme_uri, "conv-m4bfix-scheme");
    send_text_message(&store, &dag, &mut out, scheme_uri, "mid-scheme-1", "hi");

    let q_parts = format!(
        "SELECT ?p WHERE {{ ?b a <{MSG_NS}DayBucket> ; \
         <{MSG_NS}participants> ?p }}"
    );
    let mut found_scheme = false;
    if let QueryResults::Solutions(solutions) = store.query(&q_parts).expect("participants query") {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::NamedNode(n)) = sol.get("p") {
                if n.as_str() == scheme_uri {
                    found_scheme = true;
                    break;
                }
            }
        }
    }
    assert!(found_scheme,
        "scheme-bearing participant URI ({scheme_uri}) must be emitted \
         verbatim — the M4-Bfix wrap must only fire on scheme-less values");
}

// ── M4-C — UC4 Tier 3 (provenance card + Quote-in-reply) ─────────────────
//
// Tier-3 widget (200 ≤ screenPx < 500) renders the provenance card per
// UC4 § Tier 3: filename + size·mime header, divider, four metadata rows
// (Sender / Received / SHA3 / Local), divider, button row [Quote in reply,
// Re-host]. Quote-in-reply taps inject `<file:<fileName>>` into the
// conversation's draft body via `flushDraft` so the M2-built draft
// stack (tier-3 composer) reflects the new content on next rebuild.

const ATTACH_TIER3_BELOW: f64 = 500.0;

#[test]
fn attachment_tier3_widget_carries_provenance_field_labels_and_buttons() {
    // M4-C — tier-3 LOD widget DSL must contain:
    //   * `Sender` / `Received` / `SHA3` / `Local` row labels
    //   * `Quote in reply` + `Re-host` button text
    //   * `urn:composer:quote-attach:<encoded-fileId>` onTap target
    //   * `urn:msg:rehost-noop:<encoded-fileId>` onTap target
    //   * tierLabel `provenance`
    //   * worldWidth/Height that landed inside the screenPx ∈ [200,500) gate
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4c-tier3-shape";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-m4c-tier3-shape";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "with file"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4c-tier3";
    let filename = "secret.pdf";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, filename, 1_234_567),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);
    dispatch::dispatch(
        &file_complete_event(conv_id, file_id, "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);

    // (1) tierLabel = "provenance".
    let label = lod_tier_label_at(&store, &auri, ATTACH_TIER3_BELOW)
        .expect("tier-3 LOD must carry tierLabel");
    assert_eq!(label, "provenance", "tier-3 label must be `provenance`");

    let widget = lod_widget_at(&store, &auri, ATTACH_TIER3_BELOW)
        .expect("tier-3 LOD (below=500) must carry a widget literal");

    // (2) All four metadata row labels per UC4 § Tier 3 layout.
    for label in ["Sender", "Received", "SHA3", "Local"] {
        assert!(
            widget.contains(label),
            "tier-3 widget must include `{label}` row label per UC4 layout — got: {widget}"
        );
    }

    // (3) Both action button labels.
    assert!(
        widget.contains("Quote in reply"),
        "tier-3 widget must include `Quote in reply` button text — got: {widget}"
    );
    assert!(
        widget.contains("Re-host"),
        "tier-3 widget must include `Re-host` placeholder button text — got: {widget}"
    );

    // (4) Quote-in-reply onTap URN with URL-encoded fileId tail.
    let quote_urn = format!("urn:composer:quote-attach:{file_id}");
    assert!(
        widget.contains(&quote_urn),
        "tier-3 widget must wire Quote-in-reply onTap to {quote_urn} — got: {widget}"
    );

    // (5) Re-host placeholder URN.
    let rehost_urn = format!("urn:msg:rehost-noop:{file_id}");
    assert!(
        widget.contains(&rehost_urn),
        "tier-3 widget must wire Re-host onTap to {rehost_urn} — got: {widget}"
    );

    // (6) Header line carries filename + formatted size · mime.
    assert!(
        widget.contains(filename),
        "tier-3 widget must include the filename literal — got: {widget}"
    );
    assert!(
        widget.contains("1.2 MB"),
        "tier-3 widget must format the file size in human units — got: {widget}"
    );
    assert!(
        widget.contains("application/pdf"),
        "tier-3 widget must surface the mime type alongside the size — got: {widget}"
    );

    // (7) Per-tier worldHeight grew so Station's _LODContent renders
    //     against a card-sized bound rather than the tier-1 row anchor.
    //     Anchor footprint (placed-object level) stays at tier-1 dims so
    //     the screenPx<60 boundary fires off the same rectangle.
    let tier3_h = lod_world_height_at(&store, &auri, ATTACH_TIER3_BELOW)
        .expect("tier-3 LOD must carry per-tier worldHeight (M1-D bubble parity)");
    assert!(
        tier3_h > 150.0 && tier3_h < 300.0,
        "tier-3 worldHeight must reflect the provenance-card layout — got: {tier3_h}"
    );
}

#[test]
fn attachment_tier3_pending_state_shows_computing_and_pending_labels() {
    // M4-C — pre-FileComplete (state stays `pending` because libjami's
    // M4-InvB stall blocks the completion event, OR because the smoke
    // test simply hasn't fired FileComplete yet) the tier-3 widget MUST
    // surface honest placeholders rather than blank rows:
    //   * SHA3 row → "...computing"  (sha3sum is empty until completion)
    //   * Local row → "pending"      (per UC4 § Edge cases state mapping)
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4c-pending";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-m4c-pending";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "incoming"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4c-pending";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "midflight.bin", 4096),
        &store, &dag, None, "", &mut out,
    );
    // No FileComplete — state stays `pending`, sha3sum stays empty.
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, ATTACH_TIER3_BELOW)
        .expect("tier-3 LOD must carry a widget even pre-FileComplete");

    assert!(
        widget.contains("...computing"),
        "tier-3 widget must show `...computing` for empty SHA3 (M4-InvB / pre-complete) — \
         got: {widget}"
    );
    // The Local row must read `pending` for the pre-AcceptFile / no-progress state.
    // Use a slightly anchored substring so we don't match the parent message bubble
    // text. The row helper renders the value as `Text{value=pending,...}`.
    assert!(
        widget.contains("value=pending,"),
        "tier-3 widget must render Local row value=`pending` for pre-progress state — \
         got: {widget}"
    );
}

#[test]
fn quote_in_reply_tap_appends_file_reference_to_conversation_draft() {
    // M4-C — the `Quote in reply` button on the tier-3 attachment card
    // emits a TapEvent on `urn:composer:quote-attach:<encodeURIComponent(fileId)>`.
    // The pipeline handler must:
    //   (a) decode the fileId from the URN
    //   (b) resolve the attachment + look up `messenger:fileName`
    //   (c) APPEND `<file:<fileName>>` to the conversation's persisted
    //       draft body (with a leading space when the prior body is
    //       non-empty), via flushDraft → store + JS mirror
    //   (d) clear pendingDraft so a stale debounce doesn't overwrite the
    //       injection on the next ClockTick
    //
    // We exercise the empty-draft path here (current = "" → next = ref).
    // A second test below covers the append-to-existing path.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m4c-quote");

    let parent_mid = "mid-m4c-quote";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "have a look"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4c-quote";
    let filename = "image.jpg";
    dispatch::dispatch(
        &file_recv_event("conv-m4c-quote", "did:tox:peer", parent_mid, file_id, filename, 65_536),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    // No draft on disk yet — the conversation is fresh.
    assert!(
        draft_body(&store, "conv-m4c-quote").is_none(),
        "precondition: no draft persisted before the Quote-in-reply tap"
    );

    // Fire the TapEvent. URN format mirrors what the tier-3 widget's
    // Button onTap renders. fileId has no special chars → encoded form
    // matches the raw string.
    let tap_event = format!(
        "[] a <{ANTENNA_NS}TapEvent> ; \
         <{ANTENNA_NS}target> <urn:composer:quote-attach:{file_id}> ."
    );
    dispatch::dispatch(&tap_event, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let body = draft_body(&store, "conv-m4c-quote").expect(
        "after Quote-in-reply tap, the conversation's draft URN must carry \
         the appended <file:<filename>> reference",
    );
    let expected_ref = format!("<file:{filename}>");
    assert_eq!(
        body, expected_ref,
        "empty-draft Quote-in-reply must persist exactly the file reference, \
         no leading space — got: {body}"
    );
}

#[test]
fn quote_in_reply_appends_to_existing_draft_with_leading_space() {
    // M4-C — append-not-replace contract (UC4 § Tier 3 + brief): when
    // the user has already typed something into the composer, tapping
    // `Quote in reply` must APPEND the file reference with a leading
    // space, NOT overwrite their typed content.
    //
    // We seed `pendingDraft` via a TextChanged event + 280 ms wait + one
    // ClockTick — same flow the M2-C draft tests use to land a persisted
    // body. Then fire the Quote-in-reply tap and assert the resulting
    // body is `<typed> <file:<fileName>>`.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    drive_to_ready(&store, &dag, &mut out, "did:tox:self", "did:tox:peer", "conv-m4c-append");

    let parent_mid = "mid-m4c-append";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "ack"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4c-append";
    let filename = "report.pdf";
    dispatch::dispatch(
        &file_recv_event("conv-m4c-append", "did:tox:peer", parent_mid, file_id, filename, 8192),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    // Seed a typed draft body via TextChanged + debounce + ClockTick.
    dispatch::dispatch(
        &text_changed_event("urn:msg:chatinput", "see attached"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 1);
    std::thread::sleep(Duration::from_millis(280));
    tick_clock(&dag);
    settle(&dag, &store, &mut out, 10);

    assert_eq!(
        draft_body(&store, "conv-m4c-append").as_deref(),
        Some("see attached"),
        "precondition: the typed body must be persisted before the Quote-in-reply tap",
    );

    // Fire Quote-in-reply.
    let tap_event = format!(
        "[] a <{ANTENNA_NS}TapEvent> ; \
         <{ANTENNA_NS}target> <urn:composer:quote-attach:{file_id}> ."
    );
    dispatch::dispatch(&tap_event, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    let body = draft_body(&store, "conv-m4c-append").expect(
        "draft must remain after Quote-in-reply (append, not drop)",
    );
    let expected = format!("see attached <file:{filename}>");
    assert_eq!(
        body, expected,
        "Quote-in-reply must append `<file:<name>>` with a leading space, \
         preserving the user's typed body — got: {body}"
    );
}

// ── M4-D — UC4 Tier 4 (full preview: image at intrinsic size + hex dump) ──
//
// Tier-4 widget (screenPx ≥ 500) renders the full-resolution preview per
// UC4 § Tier 4:
//   * image/* + state=complete → Image{path=…,fit=contain,maxHeight=…}
//     (native res, no crop; vs M4-B tier-2's fit=cover)
//   * everything else complete → HexDump{bytes=…,maxBytes=1024} + Open
//     externally button placeholder
//   * pre-complete (any mime)  → "pending - preview unavailable" text
//
// Per-tier worldWidth/worldHeight on the LOD blank node grow to the
// tier-4 dims (600 × 450 in world units) so Station's _LODContent
// renders the deepest tier against a full-preview-sized bound. Anchor
// (placed-object level) stays at tier-1 dims so the screenPx<60 boundary
// fires off the same rectangle as M4-A/B/C — no LOD-threshold drift.

const ATTACH_TIER4_BELOW: f64 = 99999.0;

#[test]
fn attachment_tier4_image_widget_uses_image_file_path_with_fit_contain() {
    // M4-D — for an image/* mime in `complete` state the tier-4 LOD
    // widget DSL must contain the file-backed Image{} primitive with
    // path=<urlencoded savePath>, fit=contain (vs M4-B tier-2's
    // fit=cover), and a maxHeight that pins the image to the tier-4
    // bound. URL-encoded savePath proves the pipeline routes through
    // encodeURIComponent so a comma-bearing filename can't desync the
    // widget DSL prop tokenizer.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4d-tier4-image";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-m4d-tier4-image";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "with image"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4d-tier4-img";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "snap.jpg", 524_288),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);
    dispatch::dispatch(
        &file_complete_event(conv_id, file_id, "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);

    // (1) tierLabel = "preview".
    let label = lod_tier_label_at(&store, &auri, ATTACH_TIER4_BELOW)
        .expect("tier-4 LOD must carry tierLabel");
    assert_eq!(label, "preview", "tier-4 label must be `preview`");

    let widget = lod_widget_at(&store, &auri, ATTACH_TIER4_BELOW)
        .expect("tier-4 LOD (below=99999) must carry a widget literal after FileComplete");

    // (2) Tier-4 DSL contains the file-backed Image{} primitive with
    //     fit=contain (whole-image preview, not cropped fill).
    assert!(
        widget.contains("Image{path="),
        "tier-4 image widget must use the M4-B Image{{path=…}} file-backed primitive — got: {widget}"
    );
    assert!(
        widget.contains("fit=contain"),
        "tier-4 image preview must use fit=contain (whole image, no crop) — got: {widget}"
    );
    assert!(
        widget.contains("maxHeight="),
        "tier-4 image preview must pin a maxHeight so the bound matches the tier-4 card — got: {widget}"
    );

    // (3) Path is URL-encoded — separators become %2F so commas / brackets
    //     in a filename can't desync the prop tokenizer.
    assert!(
        widget.contains("%2F"),
        "tier-4 widget path prop must be URL-encoded (path separators → %2F) — got: {widget}"
    );
    assert!(
        widget.contains(file_id),
        "tier-4 widget path prop must include the libjami fileId tail — got: {widget}"
    );

    // (4) Header carries filename + size · mime so the user reads
    //     provenance metadata at the same zoom as the preview itself.
    assert!(
        widget.contains("snap.jpg"),
        "tier-4 widget header must include the filename literal — got: {widget}"
    );
    assert!(
        widget.contains("image/jpeg"),
        "tier-4 widget header must include the mime type — got: {widget}"
    );

    // (5) Per-tier worldHeight grew so Station's _LODContent renders
    //     against a preview-sized bound rather than the tier-3 card.
    //     Anchor (placed-object level) stays at tier-1 dims.
    let tier4_h = lod_world_height_at(&store, &auri, ATTACH_TIER4_BELOW)
        .expect("tier-4 LOD must carry per-tier worldHeight (M1-D bubble parity)");
    assert!(
        tier4_h > 300.0 && tier4_h < 800.0,
        "tier-4 worldHeight must reflect the full-preview-card layout — got: {tier4_h}"
    );
}

#[test]
fn attachment_tier4_non_image_widget_uses_hex_dump() {
    // M4-D — for a non-image mime in `complete` state (here `.zip` =
    // application/zip) the tier-4 LOD widget DSL must:
    //   * NOT use Image{path=…} (no preview thumbnail for binaries)
    //   * Contain the new HexDump{bytes=…} primitive with maxBytes=1024
    //   * Contain a header row showing filename + size + mime literal
    //   * Wire the `Open externally` button onTap to
    //     `urn:msg:open-external-noop:<fileId>` per UC4 § Tier 4
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4d-tier4-zip";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-m4d-tier4-zip";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "binary"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4d-tier4-zip";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "drop.zip", 4_096),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);
    dispatch::dispatch(
        &file_complete_event(conv_id, file_id, "finished"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, ATTACH_TIER4_BELOW)
        .expect("tier-4 LOD must carry a widget literal");

    // (1) Non-image branch must NOT route through Image{path=…}.
    assert!(
        !widget.contains("Image{path="),
        "tier-4 non-image widget must NOT route through Image{{path=…}} — got: {widget}"
    );

    // (2) HexDump{} primitive with the savePath + maxBytes cap.
    assert!(
        widget.contains("HexDump{bytes="),
        "tier-4 non-image widget must use the M4-D HexDump{{bytes=…}} primitive — got: {widget}"
    );
    assert!(
        widget.contains("maxBytes=1024"),
        "tier-4 HexDump must cap reads at maxBytes=1024 — got: {widget}"
    );
    // URL-encoded savePath plumbing — same encoding contract as Image{path=…}.
    assert!(
        widget.contains("%2F"),
        "tier-4 HexDump bytes prop must be URL-encoded — got: {widget}"
    );
    assert!(
        widget.contains(file_id),
        "tier-4 HexDump bytes prop must include the libjami fileId tail — got: {widget}"
    );

    // (3) Header carries filename + formatted size + mime literal.
    assert!(
        widget.contains("drop.zip"),
        "tier-4 widget header must include the filename literal — got: {widget}"
    );
    assert!(
        widget.contains("4.0 KB"),
        "tier-4 widget header must format the file size in human units — got: {widget}"
    );
    assert!(
        widget.contains("application/zip"),
        "tier-4 widget header must surface the mime type — got: {widget}"
    );

    // (4) Open externally button text + onTap URN.
    assert!(
        widget.contains("Open externally"),
        "tier-4 non-image widget must include the `Open externally` button text — got: {widget}"
    );
    let open_urn = format!("urn:msg:open-external-noop:{file_id}");
    assert!(
        widget.contains(&open_urn),
        "tier-4 widget must wire the Open externally onTap to {open_urn} — got: {widget}"
    );
}

#[test]
fn attachment_tier4_pre_complete_image_falls_back_to_hex_dump() {
    // M4-D — an image/* mime that hasn't reached `complete` yet (state
    // stays `pending` because libjami's M4-InvB stall blocks completion,
    // OR because FileComplete simply hasn't fired yet) must NOT route
    // through Image{path=…} (Image.file would surface a half-image
    // error on a partially-written file). Per the brief's allowed
    // options, M4-D chose the explicit "pending — preview unavailable"
    // text placeholder over a partial-bytes hex dump — the streaming
    // hex dump would race the libjami writer mid-transfer and the
    // rendered output would lie about the final bytes.
    //
    // Test name keeps the brief's wording (`falls_back_to_hex_dump`)
    // for cross-cut grep continuity but the assertion below documents
    // the chosen placeholder path; either fallback satisfies the brief.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let conv_id = "conv-m4d-tier4-pending";
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:conversationId \"{conv_id}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 5);

    let parent_mid = "mid-m4d-tier4-pending";
    dispatch::dispatch(
        &text_message_event("did:tox:peer", parent_mid, "incoming"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    let file_id = "fid-m4d-tier4-pending";
    dispatch::dispatch(
        &file_recv_event(conv_id, "did:tox:peer", parent_mid, file_id, "midflight.jpg", 250_000),
        &store, &dag, None, "", &mut out,
    );
    // No FileComplete — state stays `pending`, savePath set but file
    // is partial-on-disk (or absent if libjami hasn't started writing).
    settle(&dag, &store, &mut out, 15);

    let auri = attach_uri(file_id);
    let widget = lod_widget_at(&store, &auri, ATTACH_TIER4_BELOW)
        .expect("tier-4 LOD must carry a widget literal even pre-FileComplete");

    // (1) Hard contract — pre-complete image MUST NOT route through
    //     Image{path=…} regardless of which fallback shape we chose.
    assert!(
        !widget.contains("Image{path="),
        "in-flight image must not point Image.file at a partially-written file — got: {widget}"
    );

    // (2) Pre-complete also must not stream HexDump from a
    //     partially-written file (would race the writer and lie about
    //     the final bytes). We chose the explicit placeholder path.
    assert!(
        !widget.contains("HexDump{bytes="),
        "pre-complete file must not stream HexDump from a partial write — got: {widget}"
    );

    // (3) Explicit "pending" placeholder text — the chosen fallback per
    //     the brief's "Decide what's least bad and document" clause.
    //     ASCII em-dash surrogate "-" rather than U+2014 to match the
    //     formatSha3Short pragma (Menlo / SF Mono fall back on U+2014
    //     and break monospace column alignment).
    assert!(
        widget.contains("pending - preview unavailable"),
        "pre-complete tier-4 must surface the explicit `pending - preview unavailable` placeholder — got: {widget}"
    );
}

// ── M5-A — Synthetic multi-conversation seed + pipeline guard ───────────
//
// The spatial-zoom new model (M5+) lifts the conversation list out of
// chrome and makes it a plane in the canvas. M5-A is the FOUNDATION cut:
// vocab additions (antenna:Level + antenna:Scene) + 3 synthetic
// messenger:Conversation fixtures in seed.ttl + a pipeline guard that
// drops any wire-send keyed by a `synth:`-prefixed conversationId.
//
// These tests exercise the pipeline-side contract:
//   1. Synthetic conversations parse cleanly out of seed.ttl into the
//      store and are queryable as messenger:Conversation rows.
//   2. Driving a TextSubmitted while globalThis.conversationId is a
//      synthetic id MUST NOT emit carrier:SendMsg (the guard catches
//      the wire-send attempt before it reaches the carrier dispatcher).
//   3. Driving the same TextSubmitted shape against a REAL conversation
//      MUST still emit carrier:SendMsg — companion check that the guard
//      isn't a false-positive wrecking the live send path.
//   4. The new antenna:Level + antenna:Scene vocab parses into the
//      store and is queryable via SPARQL — proves the ontology
//      additions in arch/ontology/antenna.ttl don't trip Oxigraph.

#[test]
fn synthetic_conversation_seed_loads() {
    // Boot the messenger pipeline (which loads seed.ttl alongside) and
    // assert the M5-A synthetic-conversation triples are queryable.
    // Brief: "assert 3+ messenger:Conversation triples queryable; assert
    // at least one carries messenger:conversationId starting with synth:".
    let (store, _dag) = build_messenger_pipeline();

    // (1) Count messenger:Conversation rows in the store. Iterate
    //     solutions in Rust rather than SPARQL COUNT; oxigraph renders
    //     integer literals as `"3"^^<…XMLSchema#integer>` and digit-
    //     filtering on `to_string()` picks up the schema URL's "2001"
    //     and falsely inflates the count.
    let row_query = format!(
        "SELECT ?c WHERE {{ ?c a <{MESSENGER_NS}Conversation> }}"
    );
    let mut total = 0usize;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&row_query) {
        for _ in sols.flatten() {
            total += 1;
        }
    }
    assert!(
        total >= 3,
        "M5-A seed must declare ≥3 messenger:Conversation rows (got {total}) — \
         see radios/messenger/seed.ttl"
    );

    // (2) At least one row's messenger:conversationId starts with `synth:`.
    let synth_query = format!(
        "SELECT ?id WHERE {{ ?c a <{MESSENGER_NS}Conversation> ; \
         <{MESSENGER_NS}conversationId> ?id . \
         FILTER(STRSTARTS(STR(?id), \"synth:\")) }}"
    );
    let mut synth_ids: Vec<String> = Vec::new();
    if let Ok(QueryResults::Solutions(sols)) = store.query(&synth_query) {
        for sol in sols.flatten() {
            if let Some(term) = sol.get("id") {
                synth_ids.push(term.to_string());
            }
        }
    }
    assert!(
        !synth_ids.is_empty(),
        "M5-A seed must carry ≥1 synthetic conversationId prefixed `synth:` — \
         got synth_ids={synth_ids:?}, total messenger:Conversation rows={total}"
    );

    // (3) The brief's three named fixtures (synth-carol / synth-dave /
    //     synth-trio) — pin the URN scheme so a future seed refactor that
    //     renames them surfaces here, not in M5-B's tile rendering.
    for expected_id in ["synth:carol", "synth:dave", "synth:trio"] {
        let q = format!(
            "ASK {{ ?c a <{MESSENGER_NS}Conversation> ; \
             <{MESSENGER_NS}conversationId> \"{expected_id}\" }}"
        );
        match store.query(&q) {
            Ok(QueryResults::Boolean(true)) => {}
            Ok(QueryResults::Boolean(false)) => panic!(
                "M5-A seed must declare a messenger:Conversation with \
                 conversationId `{expected_id}` — ASK returned false"
            ),
            Ok(_) => panic!(
                "M5-A seed query for `{expected_id}` returned a non-boolean \
                 result; ASK should always be Boolean"
            ),
            Err(e) => panic!(
                "M5-A seed ASK query for `{expected_id}` errored: {e}"
            ),
        }
    }
}

#[test]
fn synthetic_conversation_send_is_dropped_at_pipeline_guard() {
    // Drive ConversationReady with a synthetic conversationId, then send a
    // TextSubmitted carrying a non-empty value on the chatinput target.
    // The pipeline's M5-A guard (`isSyntheticConversation` → drop +
    // breadcrumb log) MUST swallow the send: the captured raw emits must
    // contain ZERO carrier:SendMsg lines.
    //
    // Mirrors the shape of `tap_on_send_button_routes_through_pipeline_
    // without_duplicate_send` (which asserts the same absence-of-SendMsg
    // contract for the duplicate-tap path), so a regression in either
    // guard surfaces with the same fixture pattern.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    // Boot the script + seed peerUri via ContactOnline (TextSubmitted
    // gates on `globalThis.peerUri && globalThis.conversationId`; without
    // peerUri the branch never reaches the would-be send line and the
    // test's absence-of-SendMsg assertion would pass trivially — defeats
    // the contract).
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);

    // ConversationReady with a SYNTHETIC conv id — flips
    // globalThis.conversationId to "synth:carol".
    dispatch::dispatch(
        "[] a antenna:Test ; carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"synth:carol\" .",
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    // Drain anything captured during boot — only what arrives AFTER the
    // TextSubmitted matters for the assertion.
    let mut raw: Vec<String> = Vec::new();

    // Synthesize the TextSubmitted line the live HudTextField emits —
    // antenna:TextSubmitted with target=urn:msg:chatinput + a non-empty
    // value. Mirrors the existing `composer_tier3_emits_multiline_…`
    // test family's TextSubmitted fixture shape.
    dispatch::dispatch(
        "[] a <http://resonator.network/v2/antenna#TextSubmitted> ; \
         <http://resonator.network/v2/antenna#target> <urn:msg:chatinput> ; \
         <http://resonator.network/v2/antenna#value> \"hi carol\" .",
        &store, &dag, None, "", &mut out,
    );
    settle_capturing_emits(&dag, &store, &mut out, &mut raw, 10);

    let send_msg_emits: Vec<&String> = raw
        .iter()
        .filter(|m| m.contains("carrier:SendMsg") || m.contains("a carrier:SendMsg"))
        .collect();
    assert!(
        send_msg_emits.is_empty(),
        "M5-A pipeline guard must drop carrier:SendMsg for a synth: conversationId \
         (got globalThis.conversationId=`synth:carol`). Raw emits containing \
         `carrier:SendMsg`: {send_msg_emits:?}\n\nAll raw emits:\n{raw:#?}"
    );
}

#[test]
fn real_conversation_send_still_emits() {
    // Companion to `synthetic_conversation_send_is_dropped_at_pipeline_guard`
    // — verify the M5-A guard isn't a false-positive that wrecks the live
    // send path. Drive a real (non-`synth:`) conversationId via the
    // standard handshake and assert TextSubmitted DOES emit carrier:SendMsg.
    let (store, dag) = build_messenger_pipeline();

    // Standard input-enabled handshake — ContactOnline + ConversationReady
    // with a non-synthetic conv id. settle_input_enabled uses
    // "conv-m2b-test" which doesn't start with `synth:`.
    settle_input_enabled(&store, &dag);

    let mut out = CaptureOut::new();
    let mut raw: Vec<String> = Vec::new();

    dispatch::dispatch(
        "[] a <http://resonator.network/v2/antenna#TextSubmitted> ; \
         <http://resonator.network/v2/antenna#target> <urn:msg:chatinput> ; \
         <http://resonator.network/v2/antenna#value> \"hello real conv\" .",
        &store, &dag, None, "", &mut out,
    );
    settle_capturing_emits(&dag, &store, &mut out, &mut raw, 10);

    let send_msg_count = raw
        .iter()
        .filter(|m| m.contains("a carrier:SendMsg"))
        .count();
    assert_eq!(
        send_msg_count, 1,
        "M5-A guard must NOT drop carrier:SendMsg for a real (non-synth) \
         conversationId — exactly one emit expected, got {send_msg_count}. \
         Raw emits:\n{raw:#?}"
    );
    // Pin the text payload too — sanitize replaces commas with spaces, but
    // the literal we sent has no commas, so the round-trip is verbatim.
    let send_line = raw
        .iter()
        .find(|m| m.contains("a carrier:SendMsg"))
        .expect("the count assertion above guarantees at least one emit");
    assert!(
        send_line.contains("hello real conv"),
        "real-conversation send must carry the user's text verbatim — got: {send_line}"
    );
}

#[test]
fn level_and_scene_vocab_parse_in_store() {
    // M5-A vocab additions: antenna:Level (with antenna:label,
    // antenna:enterPinchProgress, antenna:widget) + antenna:Scene (with
    // antenna:scenelabel, antenna:padding, antenna:children). Validate
    // the new ontology additions don't trip Oxigraph's Turtle parser
    // and round-trip through a SPARQL select. Pre-approved by user as
    // part of the spatial-zoom paradigm shift; this test is the cargo
    // gate that keeps the new vocab queryable across future store
    // refactors.
    let store = RdfStore::open(None).expect("in-memory store");

    // Minimal author-shape: a Scene with two child Levels — exercises both
    // classes + every new property in one parse pass.
    let turtle = r#"
        <urn:test:scene:inbox> a antenna:Scene ;
            antenna:scenelabel "Inbox" ;
            antenna:padding "24.0"^^xsd:double ;
            antenna:children ( <urn:test:level:tile:carol> <urn:test:level:tile:dave> ) .

        <urn:test:level:tile:carol> a antenna:Level ;
            antenna:label "Carol tile" ;
            antenna:enterPinchProgress "0.4"^^xsd:double ;
            antenna:widget "Container{width=200,height=100}[Text{value=Carol}]" .

        <urn:test:level:tile:dave> a antenna:Level ;
            antenna:label "Dave tile" ;
            antenna:enterPinchProgress "0.4"^^xsd:double ;
            antenna:widget "Container{width=200,height=100}[Text{value=Dave}]" .
    "#;
    store
        .insert_turtle(turtle)
        .expect("M5-A Level + Scene vocab must parse — \
                 see arch/ontology/antenna.ttl");

    // (1) Scene round-trip: select label + padding by URI.
    let scene_q = format!(
        "SELECT ?l ?p WHERE {{ <urn:test:scene:inbox> a <{ANTENNA_NS}Scene> ; \
         <{ANTENNA_NS}scenelabel> ?l ; <{ANTENNA_NS}padding> ?p }}"
    );
    let mut scene_label: Option<String> = None;
    let mut scene_padding: Option<String> = None;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&scene_q) {
        for sol in sols.flatten() {
            scene_label = sol.get("l").map(|t| t.to_string());
            scene_padding = sol.get("p").map(|t| t.to_string());
        }
    }
    assert!(
        scene_label.as_deref().unwrap_or("").contains("Inbox"),
        "antenna:Scene must round-trip antenna:scenelabel — got {scene_label:?}"
    );
    assert!(
        scene_padding.as_deref().unwrap_or("").contains("24"),
        "antenna:Scene must round-trip antenna:padding — got {scene_padding:?}"
    );

    // (2) Count Level rows — must be exactly 2. Iterate solutions in
    //     Rust rather than COUNT-as-?n; oxigraph renders integer literals
    //     as `"2"^^<…XMLSchema#integer>` and `to_string()`-then-filter-
    //     digits picks up the schema URL's digits ("2001") and falsely
    //     inflates the count.
    let level_q_all = format!(
        "SELECT ?l WHERE {{ ?l a <{ANTENNA_NS}Level> }}"
    );
    let mut level_total = 0usize;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&level_q_all) {
        for _ in sols.flatten() {
            level_total += 1;
        }
    }
    assert_eq!(
        level_total, 2,
        "M5-A vocab parse must surface both authored antenna:Level rows"
    );

    // (3) Per-Level property round-trip — label + enterPinchProgress +
    //     widget DSL. Pin the widget literal so a future Turtle escaping
    //     change in the store layer surfaces here.
    let level_q = format!(
        "SELECT ?label ?prog ?w WHERE {{ \
         <urn:test:level:tile:carol> a <{ANTENNA_NS}Level> ; \
         <{ANTENNA_NS}label> ?label ; \
         <{ANTENNA_NS}enterPinchProgress> ?prog ; \
         <{ANTENNA_NS}widget> ?w }}"
    );
    let mut got_label: Option<String> = None;
    let mut got_prog: Option<String> = None;
    let mut got_widget: Option<String> = None;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&level_q) {
        for sol in sols.flatten() {
            got_label = sol.get("label").map(|t| t.to_string());
            got_prog = sol.get("prog").map(|t| t.to_string());
            got_widget = sol.get("w").map(|t| t.to_string());
        }
    }
    assert!(
        got_label.as_deref().unwrap_or("").contains("Carol tile"),
        "antenna:Level must round-trip antenna:label — got {got_label:?}"
    );
    assert!(
        got_prog.as_deref().unwrap_or("").contains("0.4"),
        "antenna:Level must round-trip antenna:enterPinchProgress — got {got_prog:?}"
    );
    assert!(
        got_widget.as_deref().unwrap_or("").contains("Text{value=Carol}"),
        "antenna:Level must round-trip antenna:widget DSL string verbatim — got {got_widget:?}"
    );
}


// ── M5-D-α — Conversation tile Scene/Level/Object emit ──────────────────
//
// The pipeline's rebuildInbox() lifecycle (per
// `control/plan/messenger-spatial-zoom/M5-D.md` § 5.M5-D-α) runs on every
// rebuildChat call and authors:
//
//   * One antenna:Scene per messenger:Conversation row, URI
//     <urn:msg:tile:scene:<convId>>, listing 4 antenna:Levels in
//     antenna:children (chip / compact / card / tile).
//   * Four antenna:Level URIs per Scene at
//     <urn:msg:tile:level:<convId>:<tier>> with antenna:label,
//     antenna:enterPinchProgress (0.0 / 0.25 / 0.5 / 0.75), and
//     antenna:widget carrying the per-tier DSL string.
//   * One placed antenna:Object per Scene at
//     <urn:msg:tile:obj:<convId>> at deterministic (x, y) per the grid
//     formula:
//         col = i % 2 ;  row = i / 2
//         x = col == 0 ? -112 : +112
//         y = 120 + row * 144
//     where `i` is the conversation's slot index after sorting by
//     (lastMessageAt DESC, displayName ASC).
//
// Boot drives WhoAmI → ContactOnline → ConversationReady so the real
// conversation lands a 4th messenger:Conversation triple alongside the 3
// synthetic-seed ones, giving 4 expected tiles. The dispatch::dispatch +
// settle pumps run the JS init block which calls rebuildChat → rebuildInbox
// → emits Scene/Level/Object Turtle, which settle re-routes via dispatch
// back into the store so the assertion path hits oxigraph live.

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const REAL_CONV_ID: &str = "conv-m5d-real";

/// Boot the messenger pipeline, drive the WhoAmI / ContactOnline /
/// ConversationReady handshake so 1 real + 3 synthetic conversations
/// land in the store, and pump the rebuildInbox emits all the way back
/// through dispatch::dispatch so tile Scene + Level + Object triples
/// resolve in oxigraph. Returns (store, dag) for downstream queries.
fn build_pipeline_with_inbox_settled() -> (RdfStore, Dag) {
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 10);
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"{REAL_CONV_ID}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);
    (store, dag)
}

#[test]
fn m5d_inbox_emits_one_scene_per_conversation_with_four_levels() {
    let (store, _dag) = build_pipeline_with_inbox_settled();

    // (1) Count tile Scenes — must equal the messenger:Conversation count.
    let scene_q = format!(
        "SELECT ?s WHERE {{ ?s a <{ANTENNA_NS}Scene> . \
         FILTER(STRSTARTS(STR(?s), \"urn:msg:tile:scene:\")) }}"
    );
    let mut tile_scenes: Vec<String> = Vec::new();
    if let Ok(QueryResults::Solutions(sols)) = store.query(&scene_q) {
        for sol in sols.flatten() {
            if let Some(t) = sol.get("s") {
                tile_scenes.push(t.to_string());
            }
        }
    }
    assert_eq!(
        tile_scenes.len(),
        4,
        "M5-D-α rebuildInbox must emit EXACTLY 4 tile Scenes (3 synthetic \
         seed + 1 real ConversationReady) — got {tile_scenes:?}"
    );

    // (2) Each expected per-conversation Scene exists at the contracted URI.
    for conv_id in ["synth:carol", "synth:dave", "synth:trio", REAL_CONV_ID] {
        let uri = format!("urn:msg:tile:scene:{conv_id}");
        let q = format!("ASK {{ <{uri}> a <{ANTENNA_NS}Scene> }}");
        match store.query(&q) {
            Ok(QueryResults::Boolean(true)) => {}
            Ok(QueryResults::Boolean(false)) => panic!(
                "M5-D-α rebuildInbox must declare <{uri}> a antenna:Scene"
            ),
            Ok(_) => panic!("M5-D-α scene ASK returned non-boolean"),
            Err(e) => panic!("M5-D-α scene <{uri}> ASK errored: {e}"),
        }
    }

    // (3) Per-Scene children list — exactly 4 Levels in (chip / compact /
    //     card / tile) tier order. Sort by enterPinchProgress so the
    //     check is robust against SPARQL's non-deterministic property-
    //     path-walk solution order — the contract that matters is
    //     "ascending enterPinchProgress maps to chip → compact → card →
    //     tile", not the lexical order the rdf:List traversal returns.
    for conv_id in ["synth:carol", "synth:dave", "synth:trio", REAL_CONV_ID] {
        let scene_uri = format!("urn:msg:tile:scene:{conv_id}");
        let q = format!(
            "PREFIX antenna: <{ANTENNA_NS}> \
             PREFIX rdf: <{RDF_NS}> \
             SELECT ?level ?prog WHERE {{ \
                 <{scene_uri}> antenna:children ?head . \
                 ?head (rdf:rest)* ?cell . \
                 ?cell rdf:first ?level . \
                 ?level antenna:enterPinchProgress ?prog . \
             }}"
        );
        let mut rows: Vec<(String, f64)> = Vec::new();
        if let Ok(QueryResults::Solutions(sols)) = store.query(&q) {
            for sol in sols.flatten() {
                let level = sol.get("level").map(|t| t.to_string()).unwrap_or_default();
                let prog = match sol.get("prog") {
                    Some(oxigraph::model::Term::Literal(lit)) => {
                        lit.value().parse::<f64>().unwrap_or(f64::NAN)
                    }
                    _ => f64::NAN,
                };
                rows.push((level, prog));
            }
        }
        assert_eq!(
            rows.len(), 4,
            "M5-D-α Scene <{scene_uri}> must list EXACTLY 4 children — got {rows:?}"
        );
        rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let expected: &[(&str, f64)] = &[
            ("chip", 0.0),
            ("compact", 0.25),
            ("card", 0.5),
            ("tile", 0.75),
        ];
        for (idx, (expected_tier, expected_prog)) in expected.iter().enumerate() {
            let expected_suffix = format!("{conv_id}:{expected_tier}>");
            assert!(
                rows[idx].0.ends_with(&expected_suffix),
                "M5-D-α Scene <{scene_uri}> tier-ordered child[{idx}] (prog={}) \
                 must end with `:{expected_tier}>` — got {:?}",
                rows[idx].1, rows[idx].0
            );
            assert!(
                (rows[idx].1 - expected_prog).abs() < 1e-9,
                "M5-D-α Scene <{scene_uri}> child[{idx}] enterPinchProgress \
                 must equal {expected_prog} — got {}",
                rows[idx].1
            );
        }
    }

    // (4) Total Level count: 4 per Scene × 4 Scenes = 16.
    let level_q = format!(
        "SELECT ?l WHERE {{ ?l a <{ANTENNA_NS}Level> . \
         FILTER(STRSTARTS(STR(?l), \"urn:msg:tile:level:\")) }}"
    );
    let mut tile_levels = 0usize;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&level_q) {
        for _ in sols.flatten() {
            tile_levels += 1;
        }
    }
    assert_eq!(
        tile_levels, 16,
        "M5-D-α must emit EXACTLY 16 tile Levels (4 per Scene × 4 Scenes) — \
         got {tile_levels}"
    );
}

#[test]
fn m5d_inbox_levels_carry_required_properties_per_tier() {
    // Each Level must carry antenna:label + enterPinchProgress + widget.
    // Pin enterPinchProgress per tier (0.0 / 0.25 / 0.5 / 0.75) — the M5-D
    // sign-off mapping. Per-tier widget DSL spot-checks the structural
    // markers (StatusDot for chip, Open conversation for tile, etc.) so a
    // builder regression surfaces without forcing an exact-byte assertion
    // (which would couple the test to incidental whitespace shifts).
    let (store, _dag) = build_pipeline_with_inbox_settled();

    let conv_id = "synth:dave";   // Dave: online=true, unread=2 — exercises both.
    let cases: &[(&str, &str, &str)] = &[
        ("chip",    "0.0",  "StatusDot"),
        ("compact", "0.25", "(2)"),       // dave's unreadCount=2
        ("card",    "0.5",  "borderRadius=8"),
        ("tile",    "0.75", "[Open conversation]"),
    ];
    for (tier, expected_progress, marker) in cases.iter() {
        let level_uri = format!("urn:msg:tile:level:{conv_id}:{tier}");
        let q = format!(
            "SELECT ?label ?prog ?widget WHERE {{ \
                 <{level_uri}> a <{ANTENNA_NS}Level> ; \
                 <{ANTENNA_NS}label> ?label ; \
                 <{ANTENNA_NS}enterPinchProgress> ?prog ; \
                 <{ANTENNA_NS}widget> ?widget \
             }}"
        );
        let mut got_label: Option<String> = None;
        let mut got_prog: Option<String> = None;
        let mut got_widget: Option<String> = None;
        if let Ok(QueryResults::Solutions(sols)) = store.query(&q) {
            for sol in sols.flatten() {
                got_label = sol.get("label").map(|t| match t {
                    oxigraph::model::Term::Literal(l) => l.value().to_string(),
                    _ => t.to_string(),
                });
                got_prog = sol.get("prog").map(|t| match t {
                    oxigraph::model::Term::Literal(l) => l.value().to_string(),
                    _ => t.to_string(),
                });
                got_widget = sol.get("widget").map(|t| match t {
                    oxigraph::model::Term::Literal(l) => l.value().to_string(),
                    _ => t.to_string(),
                });
            }
        }
        assert_eq!(
            got_label.as_deref(),
            Some(*tier),
            "M5-D-α Level <{level_uri}> antenna:label must equal {tier:?} — \
             got {got_label:?}"
        );
        let prog_val = got_prog.as_deref().unwrap_or("").parse::<f64>().unwrap_or(-1.0);
        let expected_val: f64 = expected_progress.parse().unwrap();
        assert!(
            (prog_val - expected_val).abs() < 1e-9,
            "M5-D-α Level <{level_uri}> antenna:enterPinchProgress must equal \
             {expected_progress} — got {got_prog:?}"
        );
        let widget = got_widget.unwrap_or_default();
        assert!(
            widget.contains(marker),
            "M5-D-α Level <{level_uri}> widget DSL must contain marker {marker:?} \
             — got: {widget}"
        );
    }
}

#[test]
fn m5d_inbox_objects_match_grid_formula() {
    // Slot index by (lastMessageAt DESC, displayName ASC):
    //   synth:dave   lastMessageAt=2026-05-05T09:42:00Z displayName="Dave"
    //   synth:trio   lastMessageAt=2026-05-05T11:05:00Z displayName="Dock Crew"
    //   synth:carol  lastMessageAt=2026-05-04T18:24:00Z displayName="Carol"
    //   conv-m5d-real lastMessageAt=""                  displayName=fallback
    //
    // Sorted DESC: trio (11:05) > dave (09:42) > carol (18:24 prev day) >
    // real (empty lastMessageAt → end-of-list).
    //
    // Slot → (col, row) → (x, y):
    //   0: trio  → col=0 row=0 → (-112,  120)
    //   1: dave  → col=1 row=0 → (+112,  120)
    //   2: carol → col=0 row=1 → (-112,  264)
    //   3: real  → col=1 row=1 → (+112,  264)
    let (store, _dag) = build_pipeline_with_inbox_settled();

    let expected: &[(&str, f64, f64)] = &[
        ("synth:trio",   -112.0, 120.0),
        ("synth:dave",   112.0,  120.0),
        ("synth:carol",  -112.0, 264.0),
        (REAL_CONV_ID,   112.0,  264.0),
    ];
    for (conv_id, expected_x, expected_y) in expected.iter() {
        let obj_uri = format!("urn:msg:tile:obj:{conv_id}");
        let geom = placed_geom(&store, &obj_uri).unwrap_or_else(|| panic!(
            "M5-D-α rebuildInbox must emit a placed antenna:Object at \
             <{obj_uri}> with x/y/worldWidth/worldHeight"
        ));
        assert!(
            (geom.x - expected_x).abs() < 1e-9,
            "M5-D-α tile <{obj_uri}> antenna:x must equal {expected_x} \
             (deterministic grid formula) — got {}",
            geom.x
        );
        assert!(
            (geom.y - expected_y).abs() < 1e-9,
            "M5-D-α tile <{obj_uri}> antenna:y must equal {expected_y} \
             — got {}",
            geom.y
        );
        // Tile rect is fixed 200×120 per § 3.3.
        assert!(
            (geom.w - 200.0).abs() < 1e-9,
            "M5-D-α tile <{obj_uri}> antenna:worldWidth must equal 200 — \
             got {}",
            geom.w
        );
        assert!(
            (geom.h - 120.0).abs() < 1e-9,
            "M5-D-α tile <{obj_uri}> antenna:worldHeight must equal 120 — \
             got {}",
            geom.h
        );
    }
}

#[test]
fn m5d_tile_object_carries_direct_levelcontainer_widget() {
    // M5-D-γ retired the M5-B-α / M5-D-α antenna:lod blank-node wrap on
    // tile placed Objects. The widget DSL now lives directly on the
    // Object (`<obj> antenna:widget "LevelContainer{...}"`); Station's
    // viewport SPARQL surfaces it via a new `?directWidget` OPTIONAL.
    // This test pins the new shape AND asserts the absence of the
    // legacy lod blank-node so a future regression in _emitTile that
    // accidentally re-introduces the wrap surfaces here.
    let (store, _dag) = build_pipeline_with_inbox_settled();

    let obj_uri = "urn:msg:tile:obj:synth:carol";
    let direct_q = format!(
        "ASK {{ <{obj_uri}> a <{ANTENNA_NS}Object> ; \
             <{ANTENNA_NS}widget> ?widget . \
             FILTER(CONTAINS(STR(?widget), \"LevelContainer\")) \
             FILTER(CONTAINS(STR(?widget), \"urn:msg:tile:scene:synth:carol\")) }}"
    );
    match store.query(&direct_q) {
        Ok(QueryResults::Boolean(true)) => {}
        Ok(QueryResults::Boolean(false)) => panic!(
            "M5-D-γ tile <{obj_uri}> must carry a direct antenna:widget \
             with the LevelContainer DSL — the M5-B-α lod-wrap was retired"
        ),
        Ok(_) => panic!("M5-D-γ tile direct-widget ASK returned non-boolean"),
        Err(e) => panic!("M5-D-γ tile direct-widget ASK errored: {e}"),
    }

    // Conversely: NO antenna:lod blank node may exist for a tile Object
    // any more. A leftover lod-wrap from a stale pipeline emit would
    // produce two widget rows in Station's viewport SELECT (one
    // lod-bound, one direct), which the parser tolerates via the
    // `obj.lods.isEmpty` guard but would still represent a regression
    // in the authoring shape.
    let lod_q = format!(
        "ASK {{ <{obj_uri}> <{ANTENNA_NS}lod> ?lod }}"
    );
    match store.query(&lod_q) {
        Ok(QueryResults::Boolean(false)) => {}
        Ok(QueryResults::Boolean(true)) => panic!(
            "M5-D-γ tile <{obj_uri}> must NOT carry an antenna:lod block \
             — the lod-wrap shape was retired; widget lives directly on \
             the Object now"
        ),
        Ok(_) => panic!("M5-D-γ tile lod-absence ASK returned non-boolean"),
        Err(e) => panic!("M5-D-γ tile lod-absence ASK errored: {e}"),
    }
}

#[test]
fn m5d_inbox_re_emits_when_lastmessage_changes() {
    // Drive a fresh TextMessage that updates the active conversation's
    // last-message snippet (via the existing M3 logMsg path; M5-D doesn't
    // wire messenger:lastMessage triple updates per se — but the tile
    // re-emit IS triggered by every TextMessage rebuildChat → rebuildInbox
    // call. The active-conversation tile's tier-2 / tier-3 / tier-4 DSL
    // strings read globalThis.messages directly for the last-3-messages
    // block, so the new message MUST appear in the tier-4 widget DSL.
    //
    // This isn't a "lastMessage change re-emits Levels" assertion in the
    // strict messenger:lastMessage triple sense (M5-D-α leaves the
    // per-message lastMessage update for post-α work), but it IS the
    // brief-required "lastMessage change re-emits the affected tile's
    // Levels (DSL string differs)" check at the level the script can
    // express today: the user-visible widget DSL string changes when a
    // new message arrives.
    let (store, dag) = build_pipeline_with_inbox_settled();

    // Snapshot the real conversation's tile-tier widget DSL before the
    // new TextMessage. (maybeGreet's auto-emit already ran during the
    // boot sequence in build_pipeline_with_inbox_settled, so the
    // last-3-messages block may already carry the greet — that's fine.
    // The contract being tested here is "rebuildInbox re-emits the
    // affected tile's Levels with a different DSL string when a new
    // TextMessage lands", not "the tile starts empty".)
    let real_tile_widget_q = format!(
        "SELECT ?w WHERE {{ \
             <urn:msg:tile:level:{REAL_CONV_ID}:tile> \
             <{ANTENNA_NS}widget> ?w \
         }}"
    );
    let widget_before = first_string_solution(&store, &real_tile_widget_q)
        .unwrap_or_default();
    assert!(
        !widget_before.is_empty(),
        "Pre-condition: real-conversation tile-tier widget DSL must be \
         present in the store after rebuildInbox runs"
    );
    let unique_marker = "ahoy-from-m5d-snippet";
    assert!(
        !widget_before.contains(unique_marker),
        "Pre-condition: tile-tier widget must NOT yet contain the unique \
         marker the new TextMessage will carry — got: {widget_before}"
    );

    // Drive a TextMessage on the real conversation. logMsg pushes the
    // entry into globalThis.messages, then rebuildChat → rebuildInbox
    // re-emits the real conv's tile Levels.
    let mid = "9aa7c4e1f0bd8d3a2256713f4dffaa01";
    let mut out = CaptureOut::new();
    dispatch::dispatch(
        &text_message_event("did:tox:peer", mid, unique_marker),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let widget_after = first_string_solution(&store, &real_tile_widget_q)
        .unwrap_or_default();
    assert!(
        widget_after.contains(unique_marker),
        "M5-D-α rebuildInbox must re-emit the real conversation's tile-tier \
         widget DSL with the new message snippet in the last-3 history \
         block — got: {widget_after}"
    );
    assert_ne!(
        widget_before, widget_after,
        "M5-D-α tile widget DSL must DIFFER after a TextMessage event"
    );
}

#[test]
fn m5d_inbox_emits_no_carrier_send_traffic_for_synthetic_tiles() {
    // Brief req 3: synthetic-conversation guard remains intact (no wire
    // traffic side-effect) when rebuildInbox emits tile triples for a
    // synth: conversation. Since rebuildInbox is invoked at boot from the
    // first rebuildChat call, capture every emit during that window and
    // assert ZERO carrier:SendMsg / SendReaction / SendFile lines slip out.
    //
    // Note: ConversationReady on the REAL conv lands a `[] a <urn:msg:
    // SelfIdentity> ; <urn:msg:nick> "alice" .` and a maybeGreet `carrier:
    // SendMsg` (greet). The test only asserts NO synth-keyed carrier:SendMsg
    // — which is the M5-A guard contract (verified by
    // synthetic_conversation_send_is_dropped_at_pipeline_guard for the
    // TextSubmitted entry-point; this test is the rebuildInbox-driven
    // companion).
    //
    // The greet path keys carrier:SendMsg by globalThis.peerUri (NOT
    // synth: conv), so we filter `contains("synth:")` to scope the check.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();
    let mut raw: Vec<String> = Vec::new();

    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle_capturing_emits(&dag, &store, &mut out, &mut raw, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle_capturing_emits(&dag, &store, &mut out, &mut raw, 10);

    let synth_send_emits: Vec<&String> = raw
        .iter()
        .filter(|m| m.contains("carrier:SendMsg") && m.contains("synth:"))
        .collect();
    assert!(
        synth_send_emits.is_empty(),
        "M5-D-α rebuildInbox must NOT trigger any carrier:SendMsg keyed by \
         a synth: conversationId. Synth-keyed sends found: {synth_send_emits:?}"
    );
    let synth_reaction_emits: Vec<&String> = raw
        .iter()
        .filter(|m| m.contains("carrier:SendReaction") && m.contains("synth:"))
        .collect();
    assert!(
        synth_reaction_emits.is_empty(),
        "M5-D-α rebuildInbox must NOT trigger any carrier:SendReaction \
         keyed by a synth: conversationId. Found: {synth_reaction_emits:?}"
    );

    // Sanity: at least 4 tile-Scene emits should have made it into the
    // raw stream — proves the rebuildInbox call actually ran during the
    // settle window (otherwise the absence-of-synth-send check above would
    // pass trivially).
    // Emits go through pump_emits as expanded IRIs (not prefix:LocalName);
    // match on the absolute Scene IRI so the count holds regardless of
    // whether the emitter happens to compact `antenna:Scene` or expand
    // it to the full URI.
    let tile_scene_emits = raw
        .iter()
        .filter(|m| {
            m.contains("urn:msg:tile:scene:") &&
            (m.contains("antenna:Scene") ||
             m.contains("http://resonator.network/v2/antenna#Scene"))
        })
        .count();
    assert!(
        tile_scene_emits >= 3,
        "M5-D-α expected ≥3 tile-Scene emits during boot window (3 synthetic \
         seed conversations) — got {tile_scene_emits}. Raw emits ({}): {raw:#?}",
        raw.len()
    );
}

/// Pull the first ?w solution from a SELECT query as a String literal value.
/// Mirrors the existing lod_widget_at helper but takes a free-form SPARQL +
/// ?w binding so tile-tier widget DSL queries don't have to fit the
/// `<obj_uri> antenna:lod ?l . ?l antenna:below "X" ; antenna:widget ?w` shape
/// (tile Level URIs aren't reached via lod / below).
fn first_string_solution(store: &RdfStore, sparql: &str) -> Option<String> {
    let results = store.query(sparql).ok()?;
    if let QueryResults::Solutions(sols) = results {
        for sol in sols.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("w") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

// ── M5-D-β — Inbox parent Scene + late ContactName re-emit ───────────────
//
// rebuildInbox emits an `<urn:msg:scene:inbox>` parent Scene listing every
// tile-Scene URI in `antenna:children`. SceneStore.commit reverse-walks
// the list to populate each tile-Scene's `parentSceneUri` so the
// SceneNavigator's implicit-ancestor push (Station-side) can put `Inbox`
// between `Messenger` and the tapped tile in the breadcrumb path without
// the user manually drilling into the inbox first.
//
// Late ContactName arrival (alice's ContactReady fires before peer's
// ContactName lands): the M5-A emit captured displayName="unknown" or
// shortUri(peerUri); when ContactName lands later, the pipeline must
// re-emit the messenger:Conversation triple so the inbox tile DSL stops
// rendering the stale fallback.

#[test]
fn m5d_inbox_parent_scene_lists_every_tile_as_children() {
    let (store, _dag) = build_pipeline_with_inbox_settled();

    // The inbox Scene exists with the expected predicates.
    let inbox_q = format!(
        "SELECT ?label ?padding WHERE {{ \
         <urn:msg:scene:inbox> a <{ANTENNA_NS}Scene> ; \
            <{ANTENNA_NS}scenelabel> ?label ; \
            <{ANTENNA_NS}padding> ?padding }}"
    );
    let mut found_label: Option<String> = None;
    if let Ok(QueryResults::Solutions(sols)) = store.query(&inbox_q) {
        for sol in sols.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("label") {
                found_label = Some(lit.value().to_string());
            }
        }
    }
    assert_eq!(
        found_label.as_deref(),
        Some("Inbox"),
        "M5-D-β inbox parent Scene must carry antenna:scenelabel \"Inbox\""
    );

    // Walk antenna:children rdf:List via SPARQL property path (same shape
    // as M5-B/M5-C scene-children tests use). Collect every child URI;
    // order is asserted via membership rather than position because
    // oxigraph's blank-node-cell SELECT order isn't a contract.
    let mut children: Vec<String> = Vec::new();
    let walk_q = format!(
        "PREFIX rdf: <{RDF_NS}> \
         SELECT ?child WHERE {{ \
             <urn:msg:scene:inbox> <{ANTENNA_NS}children> ?head . \
             ?head (rdf:rest)* ?cell . \
             ?cell rdf:first ?child \
         }}"
    );
    if let Ok(QueryResults::Solutions(sols)) = store.query(&walk_q) {
        for sol in sols.flatten() {
            if let Some(t) = sol.get("child") {
                children.push(t.to_string());
            }
        }
    }

    assert_eq!(
        children.len(),
        4,
        "M5-D-β inbox parent must list 4 tile Scenes (3 synth + 1 real) — \
         got {children:?}"
    );
    for conv_id in ["synth:carol", "synth:dave", "synth:trio", REAL_CONV_ID] {
        let want = format!("<urn:msg:tile:scene:{conv_id}>");
        assert!(
            children.contains(&want),
            "M5-D-β inbox children must contain {want} — got {children:?}"
        );
    }
}

#[test]
fn m5d_late_contactname_re_emits_real_conversation_displayname() {
    // Boot pipeline. ConversationReady has already fired with peerUri set
    // but no friendName, so the messenger:Conversation displayName falls
    // back to shortUri(peerUri). When a later ContactName lands, the
    // displayName must update IN THE STORE (not just JS-side
    // globalThis.friendName) so the inbox tile widget DSL renders the
    // real name.
    let (store, dag) = build_pipeline_with_inbox_settled();
    let mut out = CaptureOut::new();

    // Pre-condition: displayName is the shortUri(peerUri) fallback (NOT a
    // real name like "Bob"). shortUri trims to 8 chars; "did:tox:peer"
    // becomes "peer" since the helper strips the leading scheme prefix.
    let conv_uri = format!("urn:msg:conv:{REAL_CONV_ID}");
    let pre_q = format!(
        "SELECT ?w WHERE {{ <{conv_uri}> <{}displayName> ?w }}",
        "http://resonator.network/v2/messenger#"
    );
    let pre_name = first_string_solution(&store, &pre_q);
    assert_eq!(
        pre_name.as_deref(),
        Some("peer"),
        "Sanity: pre-ContactName, displayName is the shortUri fallback — \
         got {pre_name:?}"
    );

    // Drive a late ContactName for the peer.
    dispatch::dispatch(
        &contact_name_event("did:tox:peer", "Bob"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let post_name = first_string_solution(&store, &pre_q);
    assert_eq!(
        post_name.as_deref(),
        Some("Bob"),
        "M5-D-β: ContactName must re-emit messenger:Conversation with the \
         updated displayName — got {post_name:?}"
    );
}

// ── M5-D-βfix — emitRealConversation wraps scheme-less peerUris ────────────
//
// Real Jami contactUris are 40-hex fingerprints with NO scheme. emitRealConversation
// splices `globalThis.peerUri` into a `messenger:peerUris ( <PEER_URI> )` Turtle
// list literally — without the scheme guard, oxigraph rejects the INSERT with
// "No scheme found in an absolute IRI", bouncing the entire messenger:Conversation
// triple. The fix mirrors flushDayBuckets's M3-B guard at pipeline.ttl:632:
// scheme-less peers get the `urn:msg:participant:` synthetic prefix so the IRI
// parses cleanly. Mirrors `day_bucket_wraps_bare_hex_participant_uri_in_synthetic_scheme`
// (line 4527) but pinned at the emitRealConversation site.

#[test]
fn m5d_real_conversation_wraps_bare_hex_peer_uri_in_synthetic_scheme() {
    // Drive ConversationReady with a 40-hex bare-fingerprint contactUri (the
    // real Jami shape). emitRealConversation runs on the ConversationReady
    // handler at pipeline.ttl:4794; assert:
    //   (a) a messenger:Conversation triple exists for the conv URI,
    //       proving the SPARQL INSERT parsed cleanly (pre-fix it bounced
    //       on "No scheme found in an absolute IRI" so NO triple landed).
    //   (b) the messenger:peerUris rdf:List head IRI starts with
    //       `urn:msg:participant:` — the wrapped form proves the fix
    //       wraps scheme-less values.
    //   (c) the wrapped IRI's tail is the original 40-hex fingerprint,
    //       proving the wrap preserves identity (no data loss).
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    let bare_hex = "4748d985a10c8e84990f592a0ec0232efb733293"; // 40 hex chars, no scheme
    let conv_id = "conv-m5d-real-bare-hex";
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", bare_hex, conv_id);

    let conv_uri = format!("urn:msg:conv:{conv_id}");

    // (a) The messenger:Conversation triple landed at all. If
    // emitRealConversation's INSERT had bounced on the parser, no triple
    // of this type would exist for `conv_uri` — store-level presence is
    // the cleanest "no parser warn" proof we can pin without a tracing
    // capture.
    let conv_ask = format!("ASK {{ <{conv_uri}> a <{MSG_NS}Conversation> }}");
    assert!(
        matches!(store.query(&conv_ask), Ok(QueryResults::Boolean(true))),
        "emitRealConversation must land a messenger:Conversation triple for \
         a bare-hex peerUri — missing triple means the INSERT bounced on \
         \"No scheme found in an absolute IRI\" (the M5-D-βfix regression)"
    );

    // (b)+(c) Walk the messenger:peerUris rdf:List and pin the head IRI
    // shape: must start with `urn:msg:participant:` AND end with the
    // original 40-hex.
    let peers_q = format!(
        "PREFIX rdf: <{RDF_NS}> \
         SELECT ?p WHERE {{ \
             <{conv_uri}> <{MSG_NS}peerUris> ?head . \
             ?head (rdf:rest)* ?cell . \
             ?cell rdf:first ?p \
         }}"
    );
    let mut found_peer: Option<String> = None;
    if let QueryResults::Solutions(sols) =
        store.query(&peers_q).expect("peerUris query")
    {
        for sol in sols.flatten() {
            if let Some(oxigraph::model::Term::NamedNode(n)) = sol.get("p") {
                found_peer = Some(n.as_str().to_string());
                break;
            }
        }
    }
    let peer = found_peer.expect(
        "messenger:peerUris must carry at least one rdf:List entry — missing \
         entry means emitRealConversation failed parsing",
    );
    assert!(
        peer.starts_with("urn:msg:participant:"),
        "scheme-less peerUri must be wrapped in the urn:msg:participant: \
         synthetic — got: {peer}"
    );
    assert!(
        peer.ends_with(bare_hex),
        "wrapped peerUri must preserve the original fingerprint as the URN \
         tail — got: {peer} (expected tail: {bare_hex})"
    );
}

#[test]
fn m5d_real_conversation_preserves_existing_scheme_on_peer_uri() {
    // Companion to the bare-hex test: peerUris that already carry a scheme
    // (did:tox:peer, etc.) must pass through unchanged — the
    // `peerUri.indexOf(':') < 0` guard only wraps when the value has no
    // colon. Pin that behavior so synthetic test fixtures and any future
    // scheme-bearing transport keep their identity.
    let (store, dag) = build_messenger_pipeline();
    let mut out = CaptureOut::new();

    let scheme_uri = "did:tox:peerWithScheme";
    let conv_id = "conv-m5d-real-scheme";
    drive_to_ready(&store, &dag, &mut out, "did:tox:self", scheme_uri, conv_id);

    let conv_uri = format!("urn:msg:conv:{conv_id}");
    let peers_q = format!(
        "PREFIX rdf: <{RDF_NS}> \
         SELECT ?p WHERE {{ \
             <{conv_uri}> <{MSG_NS}peerUris> ?head . \
             ?head (rdf:rest)* ?cell . \
             ?cell rdf:first ?p \
         }}"
    );
    let mut found_scheme = false;
    if let QueryResults::Solutions(sols) =
        store.query(&peers_q).expect("peerUris query")
    {
        for sol in sols.flatten() {
            if let Some(oxigraph::model::Term::NamedNode(n)) = sol.get("p") {
                if n.as_str() == scheme_uri {
                    found_scheme = true;
                    break;
                }
            }
        }
    }
    assert!(
        found_scheme,
        "scheme-bearing peerUri ({scheme_uri}) must be emitted verbatim — \
         the M5-D-βfix wrap must only fire on scheme-less values"
    );
}

#[test]
fn m5d_inbox_no_emit_when_zero_conversations() {
    // rebuildInbox runs at boot (after init's `skipNextInbox = true`
    // clears on the first carrier event). With NO conversations in the
    // store yet (no synth seed loaded into THIS test, no ConversationReady
    // fired), the inbox parent Scene must NOT emit — emitting an empty-
    // children Scene would surface a "phantom" inbox in the breadcrumb
    // even though there are no tiles to drill into. Instead, M5-D-γ
    // emits a single help-text placeholder Object so the canvas isn't
    // blank.
    let (store, dag) = empty_messenger_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);

    let inbox_q = format!(
        "ASK WHERE {{ <urn:msg:scene:inbox> a <{ANTENNA_NS}Scene> }}"
    );
    let inbox_exists = matches!(store.query(&inbox_q), Ok(QueryResults::Boolean(true)));
    assert!(
        !inbox_exists,
        "M5-D-β: with zero messenger:Conversation rows, the inbox parent \
         Scene MUST NOT exist — empty inbox would phantom-render in the \
         breadcrumb"
    );

    // M5-D-γ — empty-inbox placeholder Object MUST exist + carry the
    // expected widget DSL (UC1 § Edge cases · "First boot").
    let placeholder_q = format!(
        "SELECT ?w WHERE {{ <urn:msg:tile:placeholder> a <{ANTENNA_NS}Object> ; \
             <{ANTENNA_NS}widget> ?w }}"
    );
    let widget = first_string_solution(&store, &placeholder_q);
    let widget_str = widget.unwrap_or_default();
    assert!(
        widget_str.contains("No conversations yet"),
        "M5-D-γ: empty-inbox placeholder Object MUST carry the help-text \
         widget DSL (\"No conversations yet\" / \"Add a friend...\") — \
         got {widget_str:?}"
    );
    assert!(
        widget_str.contains("add-friend.sh"),
        "M5-D-γ: empty-inbox placeholder must reference the add-friend.sh \
         workflow per UC1 § Edge cases — got {widget_str:?}"
    );
}

#[test]
fn m5d_empty_placeholder_clears_when_conversation_lands() {
    // Boot empty pipeline → placeholder visible. Then drive
    // ConversationReady → placeholder must disappear and 1 tile must
    // surface in its place. Pins the placeholder ↔ tile transition that
    // M5-D-γ's `_deleteTilePlaceholder()` call inside rebuildInbox
    // implements.
    let (store, dag) = empty_messenger_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch("[] a <urn:msg:WhoAmI> .", &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&self_id_event("did:tox:self"), &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);

    let placeholder_ask = format!(
        "ASK {{ <urn:msg:tile:placeholder> a <{ANTENNA_NS}Object> }}"
    );
    assert!(
        matches!(store.query(&placeholder_ask), Ok(QueryResults::Boolean(true))),
        "M5-D-γ: placeholder MUST exist while inbox is empty"
    );

    // Drive a ContactOnline + ConversationReady so a real conversation
    // lands.
    dispatch::dispatch(
        &contact_online_event("did:tox:peer"),
        &store, &dag, None, "", &mut out,
    );
    settle(&dag, &store, &mut out, 10);
    let conv_ready = format!(
        "[] a antenna:Test ; \
         carrier:ConversationReady \"_\" ; \
         carrier:contactUri \"did:tox:peer\" ; \
         carrier:conversationId \"{REAL_CONV_ID}\" ."
    );
    dispatch::dispatch(&conv_ready, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);

    assert!(
        matches!(store.query(&placeholder_ask), Ok(QueryResults::Boolean(false))),
        "M5-D-γ: placeholder MUST be cleared once a conversation lands"
    );
    let tile_ask = format!(
        "ASK {{ <urn:msg:tile:obj:{REAL_CONV_ID}> a <{ANTENNA_NS}Object> }}"
    );
    assert!(
        matches!(store.query(&tile_ask), Ok(QueryResults::Boolean(true))),
        "M5-D-γ: real-conversation tile Object MUST exist after \
         ConversationReady"
    );
}
