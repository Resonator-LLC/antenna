//! Cut 1+2 (vCard-via-spatial-zoom, 2026-05-14) — pipeline smoke for
//! `radios/messenger2/pipeline.ttl`.
//!
//! Boots the messenger2 pipeline with no peers, injects synthetic
//! `carrier:ContactOnline` + `carrier:ContactName` events, then asserts:
//!
//!   1. A per-contact `antenna:Scene` lands at `urn:msg2:contact:<uri>:scene`
//!      with `scenelabel` = display name.
//!   2. The matching `antenna:Level` lands at `urn:msg2:contact:<uri>:vlevel`
//!      with a widget DSL body that contains the contact's display name.
//!   3. The inbox parent `antenna:Scene` at `urn:msg2:inbox:scene` lists the
//!      contact scene in its children list (SceneStore reverse-walks this
//!      to set parentSceneUri for the SceneNavigator implicit-ancestor push).
//!   4. The placed `<urn:msg2:panel>` Object's widget is the LevelContainer
//!      wrap pointing at the inbox Scene (the messenger2 panel DSL now
//!      lives inside the inbox Scene's Level, not on the placed Object).
//!
//! Mirrors the shape of `messenger_pipeline_test.rs` — same Capture/dispatch
//! loop, same `__NICK__` sed pattern — so a regression in either pipeline
//! surfaces with consistent debug output.

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

/// Boot the messenger2 pipeline with placeholder substitutions applied.
/// Mirrors what `radios/messenger2/run.sh` does at launch.
fn build_messenger2_pipeline() -> (RdfStore, Dag) {
    build_messenger2_pipeline_with_auto_export("/tmp/messenger2-test/auto-export.gz")
}

/// Variant that lets a test pin the `__AUTO_EXPORT_PATH__` sed value (Cut F
/// will do the equivalent substitution Station-side from
/// `_EmbeddedRadioArgs.autoExportPath`). The default helper feeds a fixed
/// path so the onboarding tests can assert on a known value.
fn build_messenger2_pipeline_with_auto_export(auto_export_path: &str) -> (RdfStore, Dag) {
    let store = RdfStore::open(None).expect("in-memory store");

    let pipeline_raw = std::fs::read_to_string(rel("radios/messenger2/pipeline.ttl"))
        .expect("read messenger2 pipeline");
    let pipeline_ttl = pipeline_raw
        .replace("__NICK__", "alice")
        .replace("__FILES_DIR__", "/tmp/messenger2-test/files")
        .replace("__AUTO_EXPORT_PATH__", auto_export_path);
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger2 pipeline");

    let seed_ttl = std::fs::read_to_string(rel("radios/messenger2/seed.ttl"))
        .expect("read messenger2 seed");
    store.insert_turtle(&seed_ttl).expect("insert messenger2 seed");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

/// Tick the DAG until the script falls quiet — same shape as
/// `messenger_pipeline_test::settle`. Long enough for the init block to
/// fire, plus a few rounds of emit / dispatch / re-emit.
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

fn contact_online_event(contact_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:ContactOnline \"_\" ; \
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

/// Cold-start roster replay shape — carrier emits one of these per entry
/// returned by libjami::getContacts(account) at AccountReady. `display_name`
/// may be empty when the cached vCard has no FN (or is a 0-byte stub),
/// which is the specific case ISSUE-127 was opened against — trusted
/// peers were vanishing from the rail because the prior `replay_contact_names`
/// silently dropped them.
fn contact_restored_event(contact_uri: &str, display_name: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:ContactRestored \"_\" ; \
         carrier:contactUri \"{contact_uri}\" ; \
         carrier:displayName \"{display_name}\" ."
    )
}

/// Run a SPARQL ASK against the store and return whether it matched.
fn ask(store: &RdfStore, sparql: &str) -> bool {
    match store.query(sparql).expect("sparql ASK") {
        QueryResults::Boolean(b) => b,
        _ => panic!("expected ASK result, got something else"),
    }
}

/// Run a SPARQL SELECT and return rows as `Vec<Vec<String>>` of the
/// bound variables in projection order. Convenience for asserting on
/// scene/widget bindings.
fn select(store: &RdfStore, sparql: &str) -> Vec<Vec<String>> {
    let QueryResults::Solutions(rows) = store.query(sparql).expect("sparql SELECT")
    else {
        panic!("expected SELECT result");
    };
    let mut out = Vec::new();
    for row in rows {
        let row = row.expect("row");
        let vars: Vec<String> = row
            .iter()
            .map(|(_, term)| term.to_string())
            .collect();
        out.push(vars);
    }
    out
}

#[test]
fn messenger2_vcard_pipeline_emits_scene_per_contact() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    // Let the init block run + the boot rebuild() emit the empty-state
    // inbox triples.
    settle(&dag, &store, &mut out, 30);

    // ContactOnline lights up the rail. ensureContact + rebuild() emits
    // the per-contact Scene+Level + the inbox Scene. With no later
    // ContactName, the displayName falls back to shortUri(uri) — for a
    // 40-hex Jami URI that's the first 8 chars + "...".
    let alice_uri = "0123456789abcdef0123456789abcdef01234567";
    let online = contact_online_event(alice_uri);
    dispatch::dispatch(&online, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);

    // A late carrier:ContactName updates the displayName; the next
    // rebuild() re-emits the vCard Scene/Level so the store reflects
    // the new name.
    let name = contact_name_event(alice_uri, "Alice");
    dispatch::dispatch(&name, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 30);

    // (1) Per-contact Scene exists with the right type + label.
    let scene_uri = format!("urn:msg2:contact:{}:scene", alice_uri);
    let scene_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?label WHERE {{ \
               <{scene_uri}> a ant:Scene ; ant:scenelabel ?label \
             }}"
        ),
    );
    assert_eq!(
        scene_rows.len(),
        1,
        "expected exactly one Scene triple for {scene_uri}, got rows={scene_rows:?}",
    );
    let scene_label = &scene_rows[0][0];
    assert!(
        scene_label.contains("Alice"),
        "scenelabel should carry the contact display name; got {scene_label}",
    );

    // (2) Per-contact Level exists with a widget DSL carrying the name.
    let level_uri = format!("urn:msg2:contact:{}:vlevel", alice_uri);
    let level_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?widget WHERE {{ \
               <{level_uri}> a ant:Level ; ant:widget ?widget \
             }}"
        ),
    );
    assert_eq!(
        level_rows.len(),
        1,
        "expected exactly one Level triple for {level_uri}, got rows={level_rows:?}",
    );
    let widget_body = &level_rows[0][0];
    assert!(
        widget_body.contains("Alice"),
        "vCard Level widget should embed the display name; got {widget_body}",
    );

    // (3) Inbox Scene exists + its children list references the contact
    // scene (via rdf:first/rdf:rest traversal). ASK keeps the assertion
    // tight regardless of which Level / Scene URIs sit elsewhere in the
    // list (e.g. the inbox-Level URI sits first by emit order).
    assert!(
        ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
                 ASK {{ \
                   <urn:msg2:inbox:scene> a ant:Scene ; \
                     ant:children/rdf:rest*/rdf:first <{scene_uri}> \
                 }}"
            ),
        ),
        "inbox Scene's children list should contain the contact's vCard Scene",
    );

    // (4) Placed Object's widget is the LevelContainer wrap, not the raw
    // panel DSL. The rebuild() output is single-line so a substring
    // assertion is enough to anchor the wrap shape.
    let panel_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?widget WHERE {{ \
               <urn:msg2:panel> ant:lod ?lod . \
               ?lod ant:widget ?widget \
             }}"
        ),
    );
    assert_eq!(
        panel_rows.len(),
        1,
        "expected exactly one placed-Object lod widget binding, got {panel_rows:?}",
    );
    let panel_widget = &panel_rows[0][0];
    assert!(
        panel_widget.contains("LevelContainer"),
        "placed Object should render via LevelContainer; got {panel_widget}",
    );
    assert!(
        panel_widget.contains("urn:msg2:inbox:scene"),
        "LevelContainer should point at the inbox Scene; got {panel_widget}",
    );
}

// ───────────────────────────────────────────────────────────────────────────
// ISSUE-123 Cut E — onboarding pipeline tests
//
// Cover the three lifecycle points called out in the implementation plan:
//
//   1. `antenna:OnboardingRequired` re-links the seed-defined onboarding
//      placed Object to its LOD so the welcome form renders.
//   2. Tapping CREATE with a nick + autoExportPath emits a fully-formed
//      `carrier:CreateAccount` event antenna's dispatch can route.
//   3. `carrier:AccountReady` arriving while the form is live retracts
//      the onboarding lod link and re-emits the messenger panel lod via
//      the existing rebuild() path.
//
// Tests dispatch from outside the carrier (carrier=None at the dispatch
// call site) so any emitted `carrier:CreateAccount` lands in the script's
// emit stream verbatim — we sniff for it via `settle_collect_emits`
// instead of trying to observe a real CarrierClient roundtrip.
// ───────────────────────────────────────────────────────────────────────────

/// Same body as `settle` but returns every Turtle line the script emitted
/// across the tick loop. Used to assert on the exact carrier:CreateAccount
/// / carrier:ImportAccount payload — the regular `out` capture only
/// observes lines that survive `dispatch::dispatch` past the
/// `carrier=None` branch (warn-skip).
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

/// Cold-boot OnboardingRequired event — same wire shape as the
/// `bootstrap_emit` antenna publishes when `antenna_create` is invoked
/// with the empty-string sentinel (see `antenna/src/lib.rs:196`).
fn onboarding_required_event() -> String {
    "[] a antenna:OnboardingRequired ; antenna:reason \"no-account\" .".to_string()
}

/// Synthetic TapEvent matching what `widget_renderer._RegisteredTapButton`
/// emits when a Button with `onTap=<uri>` is tapped. Wrapped in
/// `a antenna:Test` so dispatch falls through to `insert_with_dag` and
/// the line reaches the script via `before_insert` (a bare
/// `a antenna:TapEvent` line would parse the same way but the test marker
/// makes it obvious in pipeline log lines that this is fabricated).
fn tap_event(target: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         antenna:TapEvent \"_\" ; \
         <http://resonator.network/v2/antenna#target> <{target}> ."
    )
}

/// Synthetic TextChanged event — same wire shape as
/// `widget_renderer._buildTextField`'s onChanged callback. The
/// `<...#target>` and `<...#value>` IRIs are spelled out so the script's
/// `extractProp(input, 'target> ')` substring match succeeds (a
/// prefix-only `antenna:target` form would lack the `>` boundary the
/// helper keys on).
fn text_changed_event(target: &str, value: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         antenna:TextChanged \"_\" ; \
         <http://resonator.network/v2/antenna#target> <{target}> ; \
         <http://resonator.network/v2/antenna#value> \"{value}\" ."
    )
}

/// Synthetic carrier:AccountReady event the script can recognise via its
/// `input.indexOf('carrier:AccountReady')` branch. `a antenna:Test`
/// avoids the carrier-dispatch warn-skip path; the carrier: properties
/// stay readable to `extractProp`.
fn account_ready_event(account_id: &str, self_uri: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         carrier:AccountReady \"_\" ; \
         carrier:account \"{account_id}\" ; \
         carrier:selfUri \"{self_uri}\" ."
    )
}

#[test]
fn onboarding_scene_emitted_on_onboarding_required() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    // Don't settle first — the script's `init` block keys onboardingActive
    // off the substring of its very first input. Dispatching
    // OnboardingRequired before any other event ensures init takes the
    // onboarding branch (rather than the "default boot, wipe the form"
    // branch), matching the production cold-boot wait-mode sequence.
    dispatch::dispatch(
        &onboarding_required_event(),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // The seed-defined onboarding Object exists statically; the
    // OnboardingRequired handler's job is to keep its lod link live (the
    // default-boot branch of init wipes the link, so we're asserting that
    // the onboarding branch did NOT wipe it / has re-emitted it).
    assert!(
        ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <urn:msg2:onboarding> ant:lod <urn:msg2:onboarding:lod> }}"
            ),
        ),
        "expected the onboarding placed Object to be linked to its LOD after \
         antenna:OnboardingRequired (default-boot wipe-path took over instead)",
    );

    // Symmetric assertion: messenger panel lod should have been wiped so
    // the seed-defined "connecting..." placeholder doesn't compete with
    // the welcome form for screen space.
    assert!(
        !ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <urn:msg2:panel> ant:lod ?l }}"
            ),
        ),
        "expected the messenger panel lod link to be wiped while onboarding \
         is active",
    );

    // Sanity: the LOD widget itself still points at the onboarding Scene
    // (the seed value, untouched by the pipeline).
    let lod_widget = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?w WHERE {{ <urn:msg2:onboarding:lod> ant:widget ?w }}"
        ),
    );
    assert_eq!(
        lod_widget.len(),
        1,
        "expected one widget binding on the onboarding LOD, got {lod_widget:?}",
    );
    assert!(
        lod_widget[0][0].contains("urn:msg2:onboarding:scene"),
        "onboarding LOD widget should reference the onboarding Scene; got {}",
        lod_widget[0][0],
    );
}

#[test]
fn create_account_tap_emits_carrier_event() {
    let auto_export = "/tmp/messenger2-test/onboarding/auto-export.gz";
    let (store, dag) = build_messenger2_pipeline_with_auto_export(auto_export);
    let mut out = CaptureOut::new();

    // Light up onboarding so the form's globalThis state is initialised
    // and the CREATE tap handler is active.
    dispatch::dispatch(
        &onboarding_required_event(),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Seed the nick TextField — the script mirrors TextChanged events
    // into globalThis.onboardingNick.
    dispatch::dispatch(
        &text_changed_event("urn:msg2:onboarding:nick", "alice"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // CMP-002 — accept the Terms (the conversational "I agree" turn); the
    // connect action is gated on it.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:agree"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Tap connect. Capture every emit drained while the script processes
    // the tap — the carrier:CreateAccount Turtle would otherwise be
    // routed into a `carrier=None` warn-skip and lost.
    dispatch::dispatch(
        &tap_event("urn:msg2:onboarding:connect"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);

    let create_emit = emits
        .iter()
        .find(|e| e.contains("carrier:CreateAccount"))
        .unwrap_or_else(|| {
            panic!(
                "expected a carrier:CreateAccount emit after tap; \
                 saw {} emits, none matching. Emits:\n  {}",
                emits.len(),
                emits.join("\n  "),
            )
        });

    assert!(
        create_emit.contains("carrier:displayName \"alice\""),
        "expected carrier:displayName \"alice\" in CreateAccount emit; got: {create_emit}",
    );
    assert!(
        create_emit.contains(&format!("carrier:autoExportPath \"{auto_export}\"")),
        "expected carrier:autoExportPath \"{auto_export}\" (seeded via __AUTO_EXPORT_PATH__) in CreateAccount emit; got: {create_emit}",
    );
}

#[test]
fn account_ready_clears_onboarding_and_renders_messenger() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    dispatch::dispatch(
        &onboarding_required_event(),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Pre-condition: onboarding is live (messenger panel wiped, onboarding
    // lod present). Skipping the assertion here — covered by test (1).

    // Simulate carrier completing the account mint. The pipeline's
    // SelfId/AccountReady branch should: clear onboardingActive,
    // unlink the onboarding Object, then run rebuild() which re-emits
    // the messenger panel lod.
    let alice_uri = "0123456789abcdef0123456789abcdef01234567";
    let alice_account = "abc123def456abc123def456abc123def456abcd";
    dispatch::dispatch(
        &account_ready_event(alice_account, alice_uri),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Messenger panel lod is back — rebuild() ran with onboardingActive=false.
    let panel_widget_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?w WHERE {{ \
               <urn:msg2:panel> ant:lod ?lod . \
               ?lod ant:widget ?w \
             }}"
        ),
    );
    assert_eq!(
        panel_widget_rows.len(),
        1,
        "expected exactly one messenger panel lod widget after AccountReady, \
         got {panel_widget_rows:?}",
    );
    assert!(
        panel_widget_rows[0][0].contains("LevelContainer"),
        "messenger panel lod widget should be the LevelContainer wrap; got {}",
        panel_widget_rows[0][0],
    );

    // Onboarding Object's lod link is gone — the form no longer renders.
    assert!(
        !ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <urn:msg2:onboarding> ant:lod ?l }}"
            ),
        ),
        "expected the onboarding Object's lod link to be retracted after \
         AccountReady cleared onboardingActive",
    );
}

#[test]
fn import_account_tap_emits_carrier_event() {
    // ISSUE-123 Cut F — exercise the IMPORT branch end-to-end. Cut E
    // deferred this case because the picker integration didn't exist
    // yet; Cut F's `_openArchivePicker` writes the picked path back as
    // a synthetic `antenna:TextChanged` targeting
    // `urn:msg2:onboarding:archive-path`, which is what this test
    // emulates. The IMPORT tap should then assemble a
    // `carrier:ImportAccount ; carrier:archivePath "..." .` emit.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch::dispatch(
        &onboarding_required_event(),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Pre-fill the archive path the way Cut F's _seedOnboardingArchivePath
    // does on the `onboardingArchivePreselected` boot path, AND the way
    // _openArchivePicker does after the user selects a file. Either
    // origin lands on the same wire shape.
    let archive_path = "/tmp/messenger2-test/onboarding/preselected-archive.gz";
    dispatch::dispatch(
        &text_changed_event("urn:msg2:onboarding:archive-path", archive_path),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // CMP-002 — accept the Terms (the conversational "I agree" turn); IMPORT
    // is gated on it.
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
        &tap_event("urn:msg2:onboarding:import"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    let emits = settle_collect_emits(&dag, &store, &mut out, 30);

    let import_emit = emits
        .iter()
        .find(|e| e.contains("carrier:ImportAccount"))
        .unwrap_or_else(|| {
            panic!(
                "expected a carrier:ImportAccount emit after IMPORT tap; \
                 saw {} emits, none matching. Emits:\n  {}",
                emits.len(),
                emits.join("\n  "),
            )
        });

    assert!(
        import_emit.contains(&format!("carrier:archivePath \"{archive_path}\"")),
        "expected carrier:archivePath \"{archive_path}\" in ImportAccount emit; got: {import_emit}",
    );
}

// ───────────────────────────────────────────────────────────────────────────
// ISSUE-127 — ContactRestored cold-start roster hydration
//
// The bug: trusted contacts whose cached vCard was empty / missing FN never
// produced a `carrier:ContactName` on cold start (the old replay loop
// silently dropped them), so the rail had no idea they existed.
//
// The fix: a dedicated `carrier:ContactRestored` event fires once per entry
// returned by `libjami::getContacts(account)` regardless of vCard state.
// The pipeline ensureContact()s on every ContactRestored, only updating
// displayName when the event carries a non-empty value — so a later real
// ContactName still wins.
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn contact_restored_with_empty_display_name_still_renders_scene() {
    // The originally-broken case: a trusted peer whose vCard has no FN
    // arrives via ContactRestored with displayName="". The contact MUST
    // appear in the store (per-contact Scene + inbox children list) —
    // displayName falls back to shortUri() for now, gets upgraded later
    // when a real vCard arrives.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    settle(&dag, &store, &mut out, 30);

    let ghost_uri = "51a5757140adc1aaa511ac55597cf53883489157";
    dispatch::dispatch(
        &contact_restored_event(ghost_uri, ""),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let scene_uri = format!("urn:msg2:contact:{}:scene", ghost_uri);
    assert!(
        ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 ASK {{ <{scene_uri}> a ant:Scene }}"
            ),
        ),
        "ContactRestored with empty displayName must still produce a contact \
         Scene — that's the whole point of ISSUE-127. scene_uri={scene_uri}",
    );

    // And it must be wired into the inbox's children list so the tile
    // actually appears in the rail.
    assert!(
        ask(
            &store,
            &format!(
                "PREFIX ant: <{ANTENNA_NS}> \
                 PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
                 ASK {{ \
                   <urn:msg2:inbox:scene> a ant:Scene ; \
                     ant:children/rdf:rest*/rdf:first <{scene_uri}> \
                 }}"
            ),
        ),
        "ContactRestored must add the peer to the inbox children list so \
         the rail tile renders. scene_uri={scene_uri}",
    );
}

#[test]
fn contact_restored_with_display_name_seeds_label() {
    // Happy path: ContactRestored arrives with a non-empty displayName
    // (cached vCard FN was present on disk). One round-trip and the Scene
    // label matches.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    settle(&dag, &store, &mut out, 30);

    let bob_uri = "4748d985a10c8e84990f592a0ec0232efb733293";
    dispatch::dispatch(
        &contact_restored_event(bob_uri, "bob"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let scene_uri = format!("urn:msg2:contact:{}:scene", bob_uri);
    let label_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?label WHERE {{ \
               <{scene_uri}> a ant:Scene ; ant:scenelabel ?label \
             }}"
        ),
    );
    assert_eq!(
        label_rows.len(),
        1,
        "expected one Scene label binding for {scene_uri}, got {label_rows:?}",
    );
    assert!(
        label_rows[0][0].contains("bob"),
        "ContactRestored displayName should land in ant:scenelabel; got {}",
        label_rows[0][0],
    );
}

#[test]
fn late_contact_name_upgrades_restored_peer_label() {
    // The flow the bug fix promises: ContactRestored seeds the peer with
    // an empty displayName, a later ContactName via Swarm sync upgrades
    // it. The rail tile should re-render with the new name.
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();
    settle(&dag, &store, &mut out, 30);

    let alice_uri = "0123456789abcdef0123456789abcdef01234567";

    // Cold-start replay: peer exists, no name yet.
    dispatch::dispatch(
        &contact_restored_event(alice_uri, ""),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    // Sanity: pre-upgrade the label MUST NOT yet say "Alice" — otherwise
    // the next assertion would pass for the wrong reason.
    let scene_uri = format!("urn:msg2:contact:{}:scene", alice_uri);
    let pre_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?label WHERE {{ \
               <{scene_uri}> a ant:Scene ; ant:scenelabel ?label \
             }}"
        ),
    );
    assert_eq!(pre_rows.len(), 1, "Scene must exist after ContactRestored");
    assert!(
        !pre_rows[0][0].contains("Alice"),
        "pre-upgrade label must not already carry the real name; got {}",
        pre_rows[0][0],
    );

    // Swarm sync delivers the vCard later — real ContactName lands.
    dispatch::dispatch(
        &contact_name_event(alice_uri, "Alice"),
        &store,
        &dag,
        None,
        "",
        &mut out,
    );
    settle(&dag, &store, &mut out, 30);

    let post_rows = select(
        &store,
        &format!(
            "PREFIX ant: <{ANTENNA_NS}> \
             SELECT ?label WHERE {{ \
               <{scene_uri}> a ant:Scene ; ant:scenelabel ?label \
             }}"
        ),
    );
    assert_eq!(post_rows.len(), 1, "Scene survives ContactName upgrade");
    assert!(
        post_rows[0][0].contains("Alice"),
        "late ContactName must upgrade scenelabel from shortUri to the real \
         name; got {}",
        post_rows[0][0],
    );
}
