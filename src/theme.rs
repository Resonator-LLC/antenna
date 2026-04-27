// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Theme resolver runner.
//!
//! The design system (`http://resonator.network/v2/design#`) describes
//! themes — colors, typestyles, icons, radii, spacings — as RDF data. The
//! resolution logic that flattens the `design:extends` chain lives as
//! SPARQL CONSTRUCT queries inside `antenna/spin/theme_resolver.spin.ttl`,
//! not in this file. This module is just a runner: it loads the SPIN file,
//! fetches the three queries from the store, executes them, and returns the
//! flat resolved triples.
//!
//! Decision **H2** (see `arch/design.md`): resolution lives as RDF +
//! SPARQL, not Rust. The runner here is a thin convenience that lets
//! callers (dispatch, tests, Station's eventual ThemeStore over the WS)
//! execute "the canonical resolver" without re-encoding the queries.

use anyhow::{anyhow, Context, Result};
use oxigraph::model::Triple;
use oxigraph::sparql::QueryResults;
use std::path::Path;

use crate::store::RdfStore;

/// `design:` vocabulary namespace IRI. Exposed so callers building SPARQL
/// strings (dispatch hooks, Station's eventual ThemeStore) can reuse the
/// same constant the resolver does.
pub const DESIGN_NS: &str = "http://resonator.network/v2/design#";

/// IRIs of the three resolver queries inside `theme_resolver.spin.ttl`.
pub const RESOLVER_QUERIES: &[&str] = &[
    "urn:resolver:theme/role-bindings",
    "urn:resolver:theme/has-token",
    "urn:resolver:theme/stroke-width",
];

/// Load the SPIN-encoded resolver definition into the store.
///
/// The resolver is just RDF data: three `sp:Construct` resources named by
/// stable URNs. Loading it makes the queries fetchable via SELECT.
pub fn load_resolver(store: &RdfStore, path: &Path) -> Result<()> {
    let ttl =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    store.insert_turtle(&ttl)?;
    Ok(())
}

/// Load resolver TTL from an in-memory string. Used by the binary entrypoint
/// where the resolver source is embedded via `include_str!` rather than read
/// from a workspace path.
pub fn load_resolver_str(store: &RdfStore, ttl: &str) -> Result<()> {
    store.insert_turtle(ttl)?;
    Ok(())
}

/// Run the full resolver pipeline against the active theme in the store.
///
/// Returns the flat resolved triples — role bindings, bound-token
/// properties, has-token entries (radii / spacing / icons), and
/// strokeWidth — as a single concatenated vector. The caller is
/// responsible for serializing them to wire format if needed.
pub fn resolve_active_theme(store: &RdfStore) -> Result<Vec<Triple>> {
    let mut out = Vec::new();
    for q_iri in RESOLVER_QUERIES {
        let text = fetch_query_text(store, q_iri)?;
        run_construct(store, &text, &mut out)?;
    }
    Ok(out)
}

/// Look up the `sp:text` literal of a resolver query by its IRI.
fn fetch_query_text(store: &RdfStore, query_iri: &str) -> Result<String> {
    let select = format!(
        "PREFIX sp: <http://spinrdf.org/sp#> \
         SELECT ?text WHERE {{ <{query_iri}> sp:text ?text }}"
    );
    let results = store.query(&select)?;
    if let QueryResults::Solutions(mut solutions) = results {
        if let Some(sol) = solutions.next() {
            let sol = sol?;
            let term = sol
                .get("text")
                .ok_or_else(|| anyhow!("?text unbound for {query_iri}"))?;
            return Ok(literal_value(term));
        }
    }
    Err(anyhow!("resolver query not found: {query_iri}"))
}

/// Extract the lexical form from any RDF term display string.
///
/// Oxigraph renders literals as `"…"`, `"…"^^<datatype>`, or `'''…'''`. The
/// resolver query texts are always plain xsd:string literals (no datatype
/// suffix in source), so this is a small unwrap rather than a full literal
/// parser.
fn literal_value(term: &oxigraph::model::Term) -> String {
    if let oxigraph::model::Term::Literal(lit) = term {
        return lit.value().to_string();
    }
    term.to_string()
}

fn run_construct(store: &RdfStore, sparql: &str, out: &mut Vec<Triple>) -> Result<()> {
    match store.query(sparql)? {
        QueryResults::Graph(triples) => {
            for t in triples {
                out.push(t?);
            }
            Ok(())
        }
        _ => Err(anyhow!("resolver query did not return a CONSTRUCT result")),
    }
}

/// Number of role bindings voidline declares: 41 color roles + 15 type
/// roles. Exposed for tests and for callers wanting to sanity-check
/// completeness; not load-bearing in the resolver itself.
#[doc(hidden)]
pub const VOIDLINE_ROLE_COUNT: usize = 56;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    const VOIDLINE_NS: &str = "http://resonator.network/v2/themes/voidline#";
    const CB_NS: &str = "http://resonator.network/v2/themes/voidline-cb-safe#";

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("antenna sits one level under the workspace root")
            .to_path_buf()
    }

    fn rel(p: &str) -> PathBuf {
        workspace_root().join(p)
    }

    /// Build a fresh in-memory store with vocab, both themes, and the
    /// resolver loaded.
    fn build_store() -> RdfStore {
        let store = RdfStore::open(None).expect("in-memory store");
        for path in [
            "arch/ontology/design.ttl",
            "themes/voidline/voidline.ttl",
            "themes/voidline-cb-safe/voidline-cb-safe.ttl",
        ] {
            let ttl = std::fs::read_to_string(rel(path)).expect("read theme file");
            store.insert_turtle(&ttl).expect("insert theme");
        }
        load_resolver(&store, &rel("antenna/spin/theme_resolver.spin.ttl"))
            .expect("load resolver");
        store
    }

    fn activate(store: &RdfStore, theme_iri: &str) {
        let upd = format!(
            "PREFIX design: <{DESIGN_NS}>
             DELETE {{ ?t design:active true }}
             INSERT {{ <{theme_iri}> design:active true }}
             WHERE  {{ ?t a design:Theme . OPTIONAL {{ ?t design:active true }} }}"
        );
        store.update(&upd).expect("activate theme");
    }

    fn iri(ns: &str, local: &str) -> String {
        format!("<{ns}{local}>")
    }

    /// Build a `{role → token}` map from `?role design:resolvesTo ?token`
    /// triples in a resolved-theme triple set.
    fn role_map(triples: &[Triple]) -> HashMap<String, String> {
        let pred = format!("<{DESIGN_NS}resolvesTo>");
        triples
            .iter()
            .filter(|t| t.predicate.to_string() == pred)
            .map(|t| (t.subject.to_string(), t.object.to_string()))
            .collect()
    }

    /// Find the first triple matching (subject, predicate).
    fn find_triple<'a>(
        triples: &'a [Triple],
        subject: &str,
        predicate: &str,
    ) -> Option<&'a Triple> {
        triples
            .iter()
            .find(|t| t.subject.to_string() == subject && t.predicate.to_string() == predicate)
    }

    // ---- canonical voidline -------------------------------------------------

    #[test]
    fn voidline_resolves_three_canonical_accents() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        for (role, expected) in [
            ("liveData", "resonanceCyan"),
            ("liveDataHover", "resonanceCyan2"),
            ("structural", "pulseMagenta"),
            ("topology", "auroraViolet"),
        ] {
            assert_eq!(
                roles.get(&iri(DESIGN_NS, role)).map(String::as_str),
                Some(iri(VOIDLINE_NS, expected).as_str()),
                "{role} → {expected}",
            );
        }
    }

    #[test]
    fn voidline_emits_status_palette() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        for (role, expected) in [
            ("statusOnline", "statusOnline"),
            ("statusPending", "statusPending"),
            ("statusOffline", "statusOffline"),
            ("statusError", "statusError"),
        ] {
            assert_eq!(
                roles.get(&iri(DESIGN_NS, role)).map(String::as_str),
                Some(iri(VOIDLINE_NS, expected).as_str()),
                "{role} resolves to canonical voidline token",
            );
        }
    }

    #[test]
    fn voidline_emits_bound_token_properties() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");

        let cyan = iri(VOIDLINE_NS, "resonanceCyan");
        let hex_pred = format!("<{DESIGN_NS}hex>");
        let hex_triple = find_triple(&triples, &cyan, &hex_pred)
            .expect("resonanceCyan design:hex triple in resolved output");
        assert!(
            hex_triple.object.to_string().contains("#5CE0E0"),
            "resonanceCyan hex: {}",
            hex_triple.object,
        );
    }

    #[test]
    fn voidline_role_count_matches_declared_total() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);
        assert_eq!(
            roles.len(),
            VOIDLINE_ROLE_COUNT,
            "voidline declares {VOIDLINE_ROLE_COUNT} role bindings; got {}",
            roles.len(),
        );
    }

    // ---- cb-safe variant ----------------------------------------------------

    #[test]
    fn cb_safe_overrides_three_direct_role_bindings() {
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        for (role, expected_token) in [
            ("structural", "pulseMagentaCb"),
            ("statusError", "statusErrorCb"),
            ("statusOnline", "statusOnlineCb"),
        ] {
            assert_eq!(
                roles.get(&iri(DESIGN_NS, role)).map(String::as_str),
                Some(iri(CB_NS, expected_token).as_str()),
                "cb-safe must rebind {role} to {expected_token}",
            );
        }
    }

    #[test]
    fn cb_safe_applies_knock_on_rebindings() {
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        // msgSentFg pinned to the original voidline statusOnline (the green
        // that's no longer bound to a status role under cb-safe).
        assert_eq!(
            roles.get(&iri(DESIGN_NS, "msgSentFg")).map(String::as_str),
            Some(iri(VOIDLINE_NS, "statusOnline").as_str()),
            "cb-safe pins msgSentFg to original voidline:statusOnline green",
        );
        // portOut explicitly rebound to the shifted magenta so wires don't
        // collapse against a stale alias.
        assert_eq!(
            roles.get(&iri(DESIGN_NS, "portOut")).map(String::as_str),
            Some(iri(CB_NS, "pulseMagentaCb").as_str()),
            "cb-safe rebinds portOut to the cb-safe magenta",
        );
    }

    #[test]
    fn cb_safe_inherits_unmodified_roles() {
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        // Roles voidline binds and cb-safe doesn't override should still
        // resolve to voidline's tokens — proof the extends* walk picks them
        // up.
        for (role, expected) in [
            ("liveData", "resonanceCyan"),
            ("topology", "auroraViolet"),
            ("canvas", "voidLine"),
            ("statusPending", "statusPending"),
            ("statusOffline", "statusOffline"),
        ] {
            assert_eq!(
                roles.get(&iri(DESIGN_NS, role)).map(String::as_str),
                Some(iri(VOIDLINE_NS, expected).as_str()),
                "{role} inherited from voidline",
            );
        }
    }

    #[test]
    fn cb_safe_emits_overridden_token_hex() {
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");

        for (token, expected_hex) in [
            ("statusErrorCb", "#FFA050"),
            ("pulseMagentaCb", "#FF4F8B"),
            ("statusOnlineCb", "#5CD8FF"),
        ] {
            let token_iri = iri(CB_NS, token);
            let hex_pred = format!("<{DESIGN_NS}hex>");
            let triple = find_triple(&triples, &token_iri, &hex_pred)
                .unwrap_or_else(|| panic!("{token} hex triple emitted"));
            assert!(
                triple.object.to_string().contains(expected_hex),
                "{token} hex: got {}, expected {expected_hex}",
                triple.object,
            );
        }
    }

    // ---- flat tokens (radii / spacing / icons) ------------------------------

    #[test]
    fn flat_tokens_emit_radii_spacing_and_icons() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");

        let name_pred = format!("<{DESIGN_NS}name>");
        let names: Vec<String> = triples
            .iter()
            .filter(|t| t.predicate.to_string() == name_pred)
            .map(|t| t.object.to_string())
            .collect();

        for needle in [
            "\"r0\"", "\"r2\"", "\"r3\"", "\"rPill\"", // radii
            "\"s1\"", "\"s4\"", "\"s12\"",             // spacing
            "\"home\"", "\"lock\"", "\"send\"",         // icons
        ] {
            assert!(
                names.iter().any(|n| n.contains(needle)),
                "expected token name {needle} in resolved hasToken output; got {names:?}",
            );
        }
    }

    #[test]
    fn icon_paths_are_emitted_with_resolved_tokens() {
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));
        let triples = resolve_active_theme(&store).expect("resolve");

        let svg_pred = format!("<{DESIGN_NS}svgPath>");
        let svg_count = triples
            .iter()
            .filter(|t| t.predicate.to_string() == svg_pred)
            .count();
        assert!(
            svg_count >= 25,
            "expected at least 25 svgPath triples (the audit-bounded icon set), got {svg_count}",
        );
    }

    // ---- strokeWidth --------------------------------------------------------

    #[test]
    fn stroke_width_inherits_through_extends() {
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");

        let pred = format!("<{DESIGN_NS}strokeWidth>");
        let cb_iri = iri(CB_NS, "voidlineCbSafe");
        let triple = find_triple(&triples, &cb_iri, &pred)
            .expect("strokeWidth resolved for active theme");
        assert!(
            triple.object.to_string().contains("1.5"),
            "strokeWidth 1.5 inherited from voidline; got {}",
            triple.object,
        );
    }

    // ---- error / edge cases ------------------------------------------------

    #[test]
    fn no_active_theme_yields_empty_resolution() {
        let store = build_store();
        // Don't activate anything. design:active true exists on voidline by
        // default in the source ttl, so we explicitly clear it.
        store
            .update(&format!(
                "PREFIX design: <{DESIGN_NS}>
                 DELETE {{ ?t design:active true }}
                 WHERE  {{ ?t design:active true }}"
            ))
            .expect("clear active");

        let triples = resolve_active_theme(&store).expect("resolve");
        assert!(
            triples.is_empty(),
            "expected zero resolved triples with no active theme; got {} triples",
            triples.len(),
        );
    }

    #[test]
    fn missing_resolver_query_errors_clearly() {
        let store = RdfStore::open(None).expect("store");
        // Don't load any resolver / themes.
        let err = resolve_active_theme(&store).unwrap_err().to_string();
        assert!(
            err.contains("urn:resolver:theme"),
            "error should name the missing query IRI; got {err}",
        );
    }

    #[test]
    fn cb_safe_does_not_leak_voidline_pulse_magenta_into_structural() {
        // Regression guard: under naive resolution that doesn't honor
        // FILTER NOT EXISTS, voidline:pulseMagenta could leak through as a
        // second resolvesTo triple alongside the cb-safe override. Assert
        // there's exactly one structural resolution.
        let store = build_store();
        activate(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");

        let resolves_to = format!("<{DESIGN_NS}resolvesTo>");
        let structural = iri(DESIGN_NS, "structural");
        let count = triples
            .iter()
            .filter(|t| {
                t.subject.to_string() == structural && t.predicate.to_string() == resolves_to
            })
            .count();
        assert_eq!(
            count, 1,
            "structural must resolve to exactly one token; got {count}",
        );
    }
}
