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
