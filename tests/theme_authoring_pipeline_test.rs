//! Integration test for the theme-authoring radio pipeline.
//!
//! Boots the same `radios/theme-authoring/pipeline.ttl` the live radio
//! uses, drives it with a hand-crafted SliderEvent, and asserts:
//!   (a) the store now carries the mutated hex on the target token,
//!   (b) handle_design re-emitted a fresh ThemeBundleComplete marker, and
//!   (c) the resolver bundle reflects the mutation under the same token IRI.
//!
//! Together these close question 1 of the plan ("Script-emitted
//! ResolveActiveTheme broadcast scope"): the bundle reaches WS subscribers
//! end-to-end after a script-side update, not just the in-process store.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use antenna::theme;
use oxigraph::sparql::QueryResults;
use std::path::PathBuf;
use std::time::Duration;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const DESIGN_NS: &str = "http://resonator.network/v2/design#";
const VOIDLINE_NS: &str = "http://resonator.network/v2/themes/voidline#";

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

/// Boot a store carrying voidline + the resolver + the canonical pipeline,
/// activate voidline via radio:hasTheme, and load the DAG.
fn build_pipeline() -> (RdfStore, Dag) {
    let store = RdfStore::open(None).expect("in-memory store");

    for path in ["arch/ontology/design.ttl", "themes/voidline/voidline.ttl"] {
        let ttl = std::fs::read_to_string(rel(path)).expect("read theme file");
        store.insert_turtle(&ttl).expect("insert theme");
    }
    theme::load_resolver(&store, &rel("antenna/spin/theme_resolver.spin.ttl"))
        .expect("load resolver");

    store
        .update(
            "PREFIX radio: <http://resonator.network/v2/radio#>
             INSERT DATA { <urn:radio:self> radio:hasTheme \
             <http://resonator.network/v2/themes/voidline#voidline> }",
        )
        .expect("set radio:hasTheme");

    let pipeline_ttl = std::fs::read_to_string(rel("radios/theme-authoring/pipeline.ttl"))
        .expect("read theme-authoring pipeline");
    store
        .insert_turtle(&pipeline_ttl)
        .expect("insert pipeline");

    let dag = Dag::load(&store).expect("load dag");
    (store, dag)
}

/// Iterate the tick loop until the script falls quiet. Each iter pumps
/// `store.query()` requests (so the JS unblocks) before draining + re-
/// dispatching emits — same ordering AntennaContext::tick uses.
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

fn slider_event(target: &str, value: f64) -> String {
    format!(
        "[] a <{ns}SliderEvent> ; <{ns}target> <{target}> ; \
         <{ns}value> \"{value}\"^^<http://www.w3.org/2001/XMLSchema#double> .",
        ns = ANTENNA_NS,
    )
}

fn current_hex(store: &RdfStore, token_iri: &str) -> Option<String> {
    let q = format!("SELECT ?h WHERE {{ <{token_iri}> <{DESIGN_NS}hex> ?h }}");
    let results = store.query(&q).ok()?;
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("h") {
                return Some(lit.value().to_string());
            }
        }
    }
    None
}

#[test]
fn slider_mutates_resonance_cyan_and_rebroadcasts_bundle() {
    let (store, dag) = build_pipeline();
    let mut out = CaptureOut::new();

    let cyan_iri = format!("{VOIDLINE_NS}resonanceCyan");
    assert_eq!(
        current_hex(&store, &cyan_iri).as_deref(),
        Some("#5CE0E0"),
        "seed hex must match the canonical voidline value",
    );

    // Drive the R channel to 0.5 → byte 0x80. G/B (E0/E0) stay intact.
    let evt = slider_event("urn:ta:cyan-r", 0.5);
    dispatch::dispatch(&evt, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    assert_eq!(
        current_hex(&store, &cyan_iri).as_deref(),
        Some("#80E0E0"),
        "resonanceCyan should mutate to #80E0E0",
    );

    assert!(
        out.messages.iter().any(|m| m.contains("ThemeBundleComplete")),
        "expected design:ThemeBundleComplete in output: {:?}",
        out.messages,
    );

    let new_hex_in_bundle = out
        .messages
        .iter()
        .any(|m| m.contains(&cyan_iri) && m.contains("#80E0E0"));
    assert!(
        new_hex_in_bundle,
        "bundle should carry mutated hex on resonanceCyan; messages = {:?}",
        out.messages,
    );
}

#[test]
fn reset_button_restores_authored_hex() {
    let (store, dag) = build_pipeline();
    let mut out = CaptureOut::new();

    let cyan_iri = format!("{VOIDLINE_NS}resonanceCyan");

    // Drive cyan-r to 0 (#00E0E0), then fire reset-cyan.
    let drive = slider_event("urn:ta:cyan-r", 0.0);
    dispatch::dispatch(&drive, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);
    assert_eq!(current_hex(&store, &cyan_iri).as_deref(), Some("#00E0E0"));

    let reset = format!(
        "[] a <{ANTENNA_NS}TapEvent> ; <{ANTENNA_NS}target> <urn:ta:reset-cyan> .",
    );
    dispatch::dispatch(&reset, &store, &dag, None, "", &mut out);
    settle(&dag, &store, &mut out, 20);

    assert_eq!(
        current_hex(&store, &cyan_iri).as_deref(),
        Some("#5CE0E0"),
        "reset must restore the authored hex",
    );
}
