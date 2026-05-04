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
fn settle(dag: &Dag, store: &RdfStore, out: &mut CaptureOut, max_iters: usize) {
    for _ in 0..max_iters {
        std::thread::sleep(Duration::from_millis(40));
        dag.pump_queries(store);
        let emits = dag.pump_emits();
        if emits.is_empty() {
            continue;
        }
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
    let chat_lod = lod_widget_at(&store, "urn:msg:chat", 99999.0)
        .expect("chat panel widget must still emit");
    assert!(
        !chat_lod.contains(&format!("urn:msg:bubble:{MID}")),
        "Path A: bubble must NOT also appear inside the chat panel widget — got: {chat_lod}"
    );
    assert!(
        chat_lod.contains("Container{height=190}"),
        "chat panel must include the 190-px bubble-area spacer — got: {chat_lod}"
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
