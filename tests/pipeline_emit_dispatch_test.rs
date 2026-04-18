//! Integration regression for ISSUE-078.
//!
//! Pipeline contract (per `control/CLAUDE.md`): script emits with a
//! `sp:` / `carrier:` type are re-dispatched through Antenna's router
//! (so SPARQL UPDATE executes, Tox commands fire). Before the fix,
//! `AntennaContext::tick` inserted emits as raw data and echoed them to
//! WS — `sp:Modify` was silently stored as a fact and `DELETE WHERE`
//! never ran.
//!
//! These tests spin up a counter-shaped pipeline (script listens on
//! `beforeInsert`, emits `sp:Modify` + raw `antenna:lod`/`antenna:widget`
//! triples) and drive it through the same emit-drain loop `tick` uses.
//! We avoid constructing a real `ToxCarrier` (too heavy; requires a Tox
//! profile) by passing `None` to `dispatch::dispatch` — safe here
//! because the counter script never emits `carrier:` types.

use antenna::channel::AntennaOut;
use antenna::dag::Dag;
use antenna::dispatch;
use antenna::store::RdfStore;
use oxigraph::sparql::QueryResults;
use std::time::Duration;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const COUNTER_BODY: &str = r#"

function extractProp(t, p) {
    var i = t.indexOf(p);
    if (i < 0) return null;
    var a = t.substring(i + p.length).trim();
    if (a.charAt(0) === '<') {
        var end = a.indexOf('>');
        return end > 0 ? a.substring(1, end) : null;
    }
    return null;
}

function deleteLod(uri) {
    emit('[] a sp:Modify ; sp:text "DELETE WHERE { <' + uri + '> <http://resonator.network/v2/antenna#lod> ?l . ?l ?p ?v }" .');
}

function insertLod(uri, lodUri, widget) {
    emit(
        '<' + uri + '> <http://resonator.network/v2/antenna#lod> <' + lodUri + '> . ' +
        '<' + lodUri + '> <http://resonator.network/v2/antenna#widget> "' + widget + '" .'
    );
}

if (typeof globalThis.count === 'undefined') {
    globalThis.count = 0;
}

if (input.indexOf('TapEvent') >= 0) {
    var target = extractProp(input, 'target> ');

    if (target === 'urn:counter:increment') {
        globalThis.count++;
    } else if (target === 'urn:counter:decrement') {
        globalThis.count--;
    } else if (target === 'urn:counter:reset') {
        globalThis.count = 0;
    } else {
        // Unknown target — no-op
    }

    deleteLod('urn:counter:panel');
    insertLod('urn:counter:panel', 'urn:counter:panel:lod', 'Text{value=' + globalThis.count + '}');
}
"#;

/// Capturing sink that records every turtle forwarded to WS (or equivalent
/// downstream transport) by dispatch.
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

/// Simulates one `AntennaContext::tick` step 5 execution: pump emits and
/// re-dispatch each through the router. Returns the number of emits drained.
fn drain_and_dispatch(dag: &Dag, store: &RdfStore, out: &mut dyn AntennaOut) -> usize {
    let emits = dag.pump_emits();
    let n = emits.len();
    for turtle in &emits {
        dispatch::dispatch(turtle, store, dag, None, out);
    }
    n
}

/// Drive the pipeline until both: the script has produced emits AND all
/// emits have been dispatched. Bounded by `max_iters` (each iter sleeps
/// briefly to give script threads time to wake on the inbox clock fd).
fn settle(dag: &Dag, store: &RdfStore, out: &mut CaptureOut, max_iters: usize) {
    for _ in 0..max_iters {
        std::thread::sleep(Duration::from_millis(40));
        if drain_and_dispatch(dag, store, out) == 0 {
            // Nothing left to pump — pipeline is quiet.
            break;
        }
    }
}

fn build_counter_pipeline() -> (RdfStore, Dag) {
    let store = RdfStore::open(None).unwrap();
    let ttl = format!(
        r#"
        @prefix antenna: <{ns}> .

        <urn:counter:src:main> a antenna:ScriptSource ;
            antenna:language "javascript" ;
            antenna:body """{body}""" .

        <urn:counter:node:main> a antenna:ScriptNode ;
            antenna:scriptSource <urn:counter:src:main> ;
            antenna:in  antenna:beforeInsert ;
            antenna:out antenna:mainOut .
        "#,
        ns = ANTENNA_NS,
        body = COUNTER_BODY,
    );
    store.insert_turtle(&ttl).unwrap();
    let dag = Dag::load(&store).unwrap();
    (store, dag)
}

/// SELECT `?widget` bound to `<urn:counter:panel:lod>`. Returns every
/// matching literal — the pre-fix bug accumulated one per click.
fn widget_literals(store: &RdfStore) -> Vec<String> {
    let results = store
        .query(
            "SELECT ?w WHERE { <urn:counter:panel:lod> <http://resonator.network/v2/antenna#widget> ?w }",
        )
        .unwrap();
    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = results {
        for sol in solutions.flatten() {
            if let Some(oxigraph::model::Term::Literal(lit)) = sol.get("w") {
                out.push(lit.value().to_string());
            }
        }
    }
    out
}

fn tap(target: &str) -> String {
    format!(
        "[] a <{ns}TapEvent> ; <{ns}target> <{target}> .",
        ns = ANTENNA_NS,
        target = target,
    )
}

#[test]
fn counter_increment_replaces_widget_exactly_once() {
    let (store, dag) = build_counter_pipeline();
    let mut out = CaptureOut::new();

    // Three increments. After each click the store must hold exactly one
    // widget literal whose value reflects the counter — this is the
    // regression the pre-fix code failed: widgets accumulated because
    // sp:Modify was never executed.
    for expected in 1..=3 {
        let turtle = tap("urn:counter:increment");
        dispatch::dispatch(&turtle, &store, &dag, None, &mut out);
        settle(&dag, &store, &mut out, 20);

        let widgets = widget_literals(&store);
        assert_eq!(
            widgets.len(),
            1,
            "after {} increments store should hold exactly one widget, got {:?}",
            expected,
            widgets,
        );
        assert!(
            widgets[0].contains(&format!("value={}", expected)),
            "widget should reflect counter={}, got {}",
            expected,
            widgets[0],
        );
    }
}

#[test]
fn counter_decrement_and_reset_mutate_store() {
    let (store, dag) = build_counter_pipeline();
    let mut out = CaptureOut::new();

    // Two increments, one decrement.
    dispatch::dispatch(&tap("urn:counter:increment"), &store, &dag, None, &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&tap("urn:counter:increment"), &store, &dag, None, &mut out);
    settle(&dag, &store, &mut out, 20);
    dispatch::dispatch(&tap("urn:counter:decrement"), &store, &dag, None, &mut out);
    settle(&dag, &store, &mut out, 20);

    let widgets = widget_literals(&store);
    assert_eq!(widgets.len(), 1, "one widget; got {:?}", widgets);
    assert!(
        widgets[0].contains("value=1"),
        "after +,+,- the counter is 1; got {}",
        widgets[0],
    );

    // Reset lands at 0.
    dispatch::dispatch(&tap("urn:counter:reset"), &store, &dag, None, &mut out);
    settle(&dag, &store, &mut out, 20);
    let widgets = widget_literals(&store);
    assert_eq!(widgets.len(), 1);
    assert!(
        widgets[0].contains("value=0"),
        "after reset the counter is 0; got {}",
        widgets[0],
    );
}

#[test]
fn unknown_type_emit_lands_as_data_no_double_insert() {
    // Regression guard: the fix must not double-insert raw RDF emits
    // (through both pump_emits and dispatch) or trigger an infinite loop.
    // We wire a script that emits a single raw RDF statement when it sees
    // a specific trigger, then count how many copies end up in the store.

    let store = RdfStore::open(None).unwrap();
    let ttl = r#"
        @prefix antenna: <http://resonator.network/v2/antenna#> .

        <urn:t:src> a antenna:ScriptSource ;
            antenna:language "javascript" ;
            antenna:body "if (input.indexOf('Trigger') >= 0) emit('<urn:probe:a> <urn:probe:kind> \"once\" .');" .

        <urn:t:node> a antenna:ScriptNode ;
            antenna:scriptSource <urn:t:src> ;
            antenna:in  antenna:beforeInsert ;
            antenna:out antenna:mainOut .
    "#;
    store.insert_turtle(ttl).unwrap();
    let dag = Dag::load(&store).unwrap();
    let mut out = CaptureOut::new();

    // Fire one trigger.
    dispatch::dispatch(
        r#"[] a <urn:t:Trigger> ."#,
        &store,
        &dag,
        None,
        &mut out,
    );
    settle(&dag, &store, &mut out, 20);

    // Exactly one probe triple in the store — raw RDF emit must not
    // duplicate on insert.
    let count_result = store
        .query("SELECT (COUNT(*) AS ?c) WHERE { <urn:probe:a> <urn:probe:kind> ?v }")
        .unwrap();
    let mut count = String::new();
    if let QueryResults::Solutions(solutions) = count_result {
        for sol in solutions.flatten() {
            if let Some(term) = sol.get("c") {
                count = term.to_string();
                break;
            }
        }
    }
    assert!(
        count.contains('1'),
        "expected exactly one probe triple, count term = {}",
        count,
    );

    // The raw RDF emit must also have been echoed to WS — preserves the
    // previous behaviour of `output.send(turtle)` for data emits.
    assert!(
        out.messages.iter().any(|m| m.contains("urn:probe:a")),
        "raw RDF emit should reach downstream WS sink",
    );
}
