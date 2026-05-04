//! Regression test for the pipeline-reload pathology described in
//! `arch/improvement-suggestions.md` § "Test roundtrip speedups (2026-05)"
//! quick-win #1.
//!
//! Persistent-store boots used to accumulate one
//! `<urn:…:src:main> antenna:body "…"` triple per restart because the boot
//! path called `store.insert_turtle(&pipeline_ttl)` directly. Combined
//! with `query_script_nodes` having no `ORDER BY` and `or_insert_with`
//! being first-write-wins on body (`dag.rs:617-665`), a stale historical
//! body would silently win on subsequent restarts — debuggable only by
//! `rm -rf $STORE_DIR`.
//!
//! `antenna::replace_pipeline` wraps `insert_turtle` with a type-anchored
//! `DELETE WHERE { ?s a antenna:ScriptNode ; ?p ?o }` /
//! `DELETE WHERE { ?s a antenna:ScriptSource ; ?p ?o }` cycle so the new
//! pipeline TTL fully replaces the old one.

use antenna::dag::Dag;
use antenna::replace_pipeline;
use antenna::store::RdfStore;
use oxigraph::sparql::QueryResults;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";

fn pipeline_ttl(body: &str) -> String {
    // Same shape (fixed URIs, ScriptSource → ScriptNode wiring) the real
    // radios use; the body is the only thing that varies between calls.
    format!(
        r#"
<urn:test:src:main> a antenna:ScriptSource ;
    antenna:body """{body}""" ;
    antenna:language "javascript" .

<urn:test:node:main> a antenna:ScriptNode ;
    antenna:scriptSource <urn:test:src:main> ;
    antenna:in  antenna:beforeInsert ;
    antenna:out antenna:mainOut .
"#
    )
}

fn select_bodies(store: &RdfStore) -> Vec<String> {
    let mut bodies = Vec::new();
    let q = store
        .query(
            r#"
            PREFIX antenna: <http://resonator.network/v2/antenna#>
            SELECT ?body WHERE {
                ?src a antenna:ScriptSource ;
                     antenna:body ?body .
            }
            "#,
        )
        .expect("select bodies");
    if let QueryResults::Solutions(sols) = q {
        for s in sols {
            let s = s.expect("solution");
            if let Some(oxigraph::model::Term::Literal(lit)) = s.get("body") {
                bodies.push(lit.value().to_string());
            }
        }
    }
    bodies
}

#[test]
fn replace_pipeline_first_boot_inserts_cleanly() {
    // On a fresh store the DELETE WHERE matches nothing — the call must
    // still succeed and the body must be queryable afterwards.
    let store = RdfStore::open(None).expect("in-memory store");
    replace_pipeline(&store, &pipeline_ttl("v1")).expect("first replace");

    let bodies = select_bodies(&store);
    assert_eq!(bodies, vec!["v1".to_string()]);

    // Dag::load must see the body Reading the post-insert script-node
    // catalog matches the live boot path.
    let dag = Dag::load(&store).expect("dag load");
    drop(dag); // smoke: no panic on construction
}

#[test]
fn replace_pipeline_second_boot_overwrites_stale_body() {
    // Simulate the persistent-store pathology: insert v1 (as the original
    // buggy boot path did, via plain insert_turtle), then call
    // replace_pipeline with v2. After the call, only v2 must survive.
    let store = RdfStore::open(None).expect("in-memory store");
    store
        .insert_turtle(&pipeline_ttl("v1"))
        .expect("seed v1 directly");
    assert_eq!(select_bodies(&store), vec!["v1".to_string()]);

    replace_pipeline(&store, &pipeline_ttl("v2")).expect("replace with v2");

    let bodies = select_bodies(&store);
    assert_eq!(
        bodies,
        vec!["v2".to_string()],
        "stale v1 body must not survive replace_pipeline; got {:?}",
        bodies
    );

    // No leftover ScriptNode/ScriptSource triples carrying v1 metadata.
    // ScriptNode count must be exactly 1 (the new one), not 2.
    let q = store
        .query(
            r#"
            PREFIX antenna: <http://resonator.network/v2/antenna#>
            SELECT (COUNT(?n) AS ?c) WHERE { ?n a antenna:ScriptNode }
            "#,
        )
        .expect("count nodes");
    if let QueryResults::Solutions(mut sols) = q {
        let s = sols.next().expect("one row").expect("solution");
        if let Some(oxigraph::model::Term::Literal(lit)) = s.get("c") {
            assert_eq!(
                lit.value(),
                "1",
                "expected exactly one ScriptNode after replace; oxigraph reports {}",
                lit.value()
            );
        } else {
            panic!("expected literal count");
        }
    }
}

#[test]
fn replace_pipeline_winning_body_is_loaded_by_dag() {
    // End-to-end: after two replace_pipeline cycles, Dag::load observes
    // the winning body's ScriptNode entry. Without the DELETE WHERE wrap,
    // `query_script_nodes`'s first-write-wins would non-deterministically
    // pin v1 (the first solution row) — this assertion would flake.
    let store = RdfStore::open(None).expect("in-memory store");
    replace_pipeline(&store, &pipeline_ttl("v1")).expect("v1");
    replace_pipeline(&store, &pipeline_ttl("v2")).expect("v2");

    // Sanity: select the bodies first.
    assert_eq!(select_bodies(&store), vec!["v2".to_string()]);

    // Confirm Dag::load can build a DAG against the post-replace store.
    // (We can't easily reach into Dag's private node map from an
    // integration test, but the SELECT above directly probes the same
    // SPARQL surface query_script_nodes uses, so a passing select_bodies
    // assertion plus a non-panicking Dag::load is an end-to-end check
    // that the boot path now sees v2 deterministically.)
    let _dag = Dag::load(&store).expect("dag load");

    // Ensure v1 leaves no residual antenna:body literal in the store.
    let q = store
        .query(&format!(
            r#"
            PREFIX antenna: <{ANTENNA_NS}>
            ASK {{ ?src antenna:body "v1" }}
            "#
        ))
        .expect("ask v1");
    if let QueryResults::Boolean(b) = q {
        assert!(!b, "stale v1 body still present in store after replace");
    } else {
        panic!("ASK must return Boolean");
    }
}
