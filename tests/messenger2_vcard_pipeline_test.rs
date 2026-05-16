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
    let store = RdfStore::open(None).expect("in-memory store");

    let pipeline_raw = std::fs::read_to_string(rel("radios/messenger2/pipeline.ttl"))
        .expect("read messenger2 pipeline");
    let pipeline_ttl = pipeline_raw
        .replace("__NICK__", "alice")
        .replace("__FILES_DIR__", "/tmp/messenger2-test/files");
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
