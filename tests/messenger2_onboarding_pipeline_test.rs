//! Conversational onboarding pipeline smoke for `radios/messenger2/pipeline.ttl`
//! (CMP-019 — the self-demonstrating offline first-run).
//!
//! On a no-account cold boot antenna publishes a single
//! `antenna:OnboardingRequired`. The pipeline turns the onboarding Level into
//! a scripted chat with a local in-app "Resonator" setup guide that renders
//! the real messaging UI offline, collects a display name + two consents
//! (Terms/EULA per CMP-002, then an explicit "connect" action), and only then
//! emits `carrier:CreateAccount` / `carrier:ImportAccount`. This file covers:
//!
//!   1. OnboardingRequired renders the guide greeting + the step-0 quick
//!      replies, and emits NO account-minting command.
//!   2. Tapping through more -> name -> agree -> connect advances the chat,
//!      keeps `carrier:CreateAccount` suppressed until consent, then emits it
//!      carrying the chosen display name.
//!   3. The import branch: attach an archive (a synthetic TextChanged) -> the
//!      guide asks for a PIN -> Import emits `carrier:ImportAccount` with the
//!      chosen archive path.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use oxigraph::sparql::QueryResults;
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
        .replace("__FILES_DIR__", "/tmp/messenger2-onboarding-test/files")
        .replace(
            "__AUTO_EXPORT_PATH__",
            "/tmp/messenger2-onboarding-test/auto-export.gz",
        );
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert messenger2 pipeline");

    let seed_ttl =
        std::fs::read_to_string(rel("radios/messenger2/seed.ttl")).expect("read messenger2 seed");
    store.insert_turtle(&seed_ttl).expect("insert messenger2 seed");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

/// Pump the DAG until the script falls quiet, returning every emitted Turtle
/// line. Mirrors `messenger2_saved_messages_test::settle_collect_emits`.
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

fn select_rows(store: &RdfStore, sparql: &str) -> Vec<Vec<String>> {
    let QueryResults::Solutions(rows) = store.query(sparql).expect("sparql SELECT") else {
        panic!("expected SELECT result");
    };
    let mut out = Vec::new();
    for row in rows {
        let row = row.expect("row");
        out.push(row.iter().map(|(_, term)| term.to_string()).collect());
    }
    out
}

/// Read the current onboarding-Level widget DSL out of the store. The
/// pipeline rewrites it via DELETE+INSERT on every turn, so exactly one
/// binding must remain.
fn onboarding_level_widget(store: &RdfStore) -> String {
    let rows = select_rows(
        store,
        "PREFIX ant: <http://resonator.network/v2/antenna#> \
         SELECT ?w WHERE { <urn:msg2:onboarding:level> ant:widget ?w }",
    );
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one onboarding-Level widget binding, got {rows:?}",
    );
    rows[0][0].clone()
}

fn onboarding_required_event() -> String {
    "[] a antenna:Test ; antenna:OnboardingRequired \"_\" ; antenna:reason \"no-account\" ."
        .to_string()
}

fn tap_event(target: &str) -> String {
    format!(
        "[] a antenna:Test ; \
         antenna:TapEvent \"_\" ; \
         <http://resonator.network/v2/antenna#target> <{target}> ."
    )
}

/// Mirror exactly what `widget_renderer.dart`'s TextField emits: the FULL-IRI
/// rdf:type `<…/antenna#TextChanged>` (NOT the prefixed `antenna:TextChanged`).
/// Using the prefixed form here would mask the pipeline's substring-match bug
/// (the nick/PIN fields silently defaulting) — so keep this full-IRI.
fn text_changed_event(target: &str, value: &str) -> String {
    format!(
        "[] a <http://resonator.network/v2/antenna#TextChanged> ; \
         <http://resonator.network/v2/antenna#target> <{target}> ; \
         <http://resonator.network/v2/antenna#value> \"{value}\" ."
    )
}

/// Feed one input event and settle, returning the emits attributable to it.
fn dispatch_event(dag: &Dag, store: &RdfStore, out: &mut CaptureOut, event: &str) -> Vec<String> {
    dispatch::dispatch(event, store, dag, None, "", out);
    settle_collect_emits(dag, store, out, 30)
}

#[test]
fn onboarding_required_renders_guide_greeting() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    dispatch_event(&dag, &store, &mut out, &onboarding_required_event());

    let widget = onboarding_level_widget(&store);
    // Honest in-app guide identity + the real chat surface.
    assert!(
        widget.contains("setup guide"),
        "onboarding header must label the guide; got: {widget}",
    );
    assert!(
        widget.contains("Resonator your setup guide"),
        "step 0 must greet from the Resonator guide; got: {widget}",
    );
    // Tap-completable: step-0 quick replies are present.
    assert!(
        widget.contains("urn:msg2:onboarding:more")
            && widget.contains("urn:msg2:onboarding:skip"),
        "step 0 must offer the 'Tell me more' / 'Skip setup' quick replies; got: {widget}",
    );
}

#[test]
fn conversational_create_is_gated_on_consent_then_emits_create_account() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    let mut pre_consent = Vec::new();
    pre_consent.extend(dispatch_event(
        &dag,
        &store,
        &mut out,
        &onboarding_required_event(),
    ));
    // step 0 -> 1
    pre_consent.extend(dispatch_event(
        &dag,
        &store,
        &mut out,
        &tap_event("urn:msg2:onboarding:more"),
    ));
    // type a name, then continue -> step 2 (Terms)
    pre_consent.extend(dispatch_event(
        &dag,
        &store,
        &mut out,
        &text_changed_event("urn:msg2:onboarding:nick", "Reviewer"),
    ));
    pre_consent.extend(dispatch_event(
        &dag,
        &store,
        &mut out,
        &tap_event("urn:msg2:onboarding:name-continue"),
    ));

    let terms_widget = onboarding_level_widget(&store);
    assert!(
        terms_widget.contains("zero tolerance")
            && terms_widget.contains("urn:msg2:onboarding:agree"),
        "the Terms turn (CMP-002) must render with an 'I agree' action; got: {terms_widget}",
    );
    assert!(
        terms_widget.contains("urn:msg2:onboarding:terms"),
        "the Terms turn (CMP-025) must offer a 'Read the Terms' link; got: {terms_widget}",
    );

    // Consent NOT given yet — nothing may have minted an account.
    assert!(
        !pre_consent.iter().any(|e| e.contains("carrier:CreateAccount")),
        "carrier:CreateAccount must NOT be emitted before the connect tap; emits:\n  {}",
        pre_consent.join("\n  "),
    );

    // Accept Terms -> connect turn.
    dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:agree"));
    let connect_widget = onboarding_level_widget(&store);
    assert!(
        connect_widget.contains("urn:msg2:onboarding:connect")
            && connect_widget.contains("urn:msg2:onboarding:import-start"),
        "the connect turn must offer create-and-connect + import; got: {connect_widget}",
    );

    // Explicit connect consent -> account minted with the chosen name.
    let connect_emits =
        dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:connect"));
    let create = connect_emits
        .iter()
        .find(|e| e.contains("carrier:CreateAccount"))
        .unwrap_or_else(|| {
            panic!(
                "the connect tap must emit carrier:CreateAccount; emits:\n  {}",
                connect_emits.join("\n  "),
            )
        });
    assert!(
        create.contains("carrier:displayName \"Reviewer\""),
        "CreateAccount must carry the chosen display name; got: {create}",
    );
}

#[test]
fn terms_link_opens_document_without_advancing() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Advance to the Terms turn via the tap-only path.
    dispatch_event(&dag, &store, &mut out, &onboarding_required_event());
    dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:skip"));

    // Tap "Read the Terms" (CMP-025): the hosted ToU/Community-Guidelines URL
    // opens externally so "the Terms" resolves to real text before acceptance.
    let emits = dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:terms"));
    assert!(
        emits
            .iter()
            .any(|e| e.contains("urn:msg:OpenExternal")
                && e.contains("https://resonator.network/terms")),
        "tapping 'Read the Terms' must open the hosted ToU via urn:msg:OpenExternal; emits:\n  {}",
        emits.join("\n  "),
    );
    // Reading the Terms is not accepting them: no account minting, and the
    // turn still offers (and requires) the explicit "I agree" action.
    assert!(
        !emits.iter().any(|e| e.contains("carrier:CreateAccount")),
        "reading the Terms must not mint an account; emits:\n  {}",
        emits.join("\n  "),
    );
    let widget = onboarding_level_widget(&store);
    assert!(
        widget.contains("urn:msg2:onboarding:agree")
            && !widget.contains("urn:msg2:onboarding:connect"),
        "the Terms turn must remain current (not advanced) after the link tap; got: {widget}",
    );
}

#[test]
fn conversational_import_attaches_then_imports() {
    let (store, dag) = build_messenger2_pipeline();
    let mut out = CaptureOut::new();

    // Advance to the connect turn via the tap-only path (default name).
    dispatch_event(&dag, &store, &mut out, &onboarding_required_event());
    dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:skip"));
    dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:agree"));

    // Choose the import branch -> guide asks for an archive file.
    dispatch_event(
        &dag,
        &store,
        &mut out,
        &tap_event("urn:msg2:onboarding:import-start"),
    );
    let pick_widget = onboarding_level_widget(&store);
    assert!(
        pick_widget.contains("urn:msg2:onboarding:pick-archive"),
        "import start must offer the archive picker; got: {pick_widget}",
    );

    // Station writes the picked path back as a synthetic TextChanged ->
    // guide advances to the PIN turn.
    dispatch_event(
        &dag,
        &store,
        &mut out,
        &text_changed_event(
            "urn:msg2:onboarding:archive-path",
            "/tmp/resonator-archive.gz",
        ),
    );
    let pin_widget = onboarding_level_widget(&store);
    assert!(
        pin_widget.contains("urn:msg2:onboarding:pin")
            && pin_widget.contains("urn:msg2:onboarding:import"),
        "after attaching an archive the guide must ask for a PIN + offer Import; got: {pin_widget}",
    );

    // Import -> carrier:ImportAccount carrying the chosen archive path.
    let import_emits =
        dispatch_event(&dag, &store, &mut out, &tap_event("urn:msg2:onboarding:import"));
    let import = import_emits
        .iter()
        .find(|e| e.contains("carrier:ImportAccount"))
        .unwrap_or_else(|| {
            panic!(
                "the import tap must emit carrier:ImportAccount; emits:\n  {}",
                import_emits.join("\n  "),
            )
        });
    assert!(
        import.contains("carrier:archivePath \"/tmp/resonator-archive.gz\""),
        "ImportAccount must carry the chosen archive path; got: {import}",
    );
}
