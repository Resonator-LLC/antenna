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

/// Named graph that holds every theme-definition triple (design ontology,
/// theme bundles, role bindings, tokens, strokeWidth). The SPIN resolver
/// scopes its walk patterns to this graph (`GRAPH <urn:design:theme> { … }`)
/// so resolution stays O(theme-graph) regardless of how much radio/swarm data
/// accumulates in the default graph. Callers that load themes or mutate
/// `design:active` / token properties at runtime MUST target this graph, or
/// the resolver won't see their writes. The `radio:hasTheme` selector is the
/// one input that stays in the default graph (the radio seed puts it there).
pub const THEME_GRAPH: &str = "urn:design:theme";

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
///
/// Loaded into THEME_GRAPH (not the default graph) so that an idempotent
/// `clear_graph(THEME_GRAPH)` + reload at boot drops any prior resolver text.
/// The persistent store is additive, so without this an older resolver version
/// (e.g. a pre-fix `?token ?tProp ?tVal` query) would linger and `fetch_query_text`
/// could pick it up — silently reintroducing the default-graph scan.
pub fn load_resolver(store: &RdfStore, path: &Path) -> Result<()> {
    let ttl =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    store.insert_turtle_to_graph(&ttl, THEME_GRAPH)?;
    Ok(())
}

/// Load resolver TTL from an in-memory string. Used by the binary entrypoint
/// where the resolver source is embedded via `include_str!` rather than read
/// from a workspace path. Loaded into THEME_GRAPH (see [`load_resolver`]).
pub fn load_resolver_str(store: &RdfStore, ttl: &str) -> Result<()> {
    store.insert_turtle_to_graph(ttl, THEME_GRAPH)?;
    Ok(())
}

/// Run the full resolver pipeline against the active theme in the store.
///
/// Returns the flat resolved triples — role bindings, bound-token
/// properties, has-token entries (radii / spacing / icons), and
/// strokeWidth — as a single concatenated vector. The caller is
/// responsible for serializing them to wire format if needed.
pub fn resolve_active_theme(store: &RdfStore) -> Result<Vec<Triple>> {
    // Resolve the active theme IRI up front, then pin it into each CONSTRUCT as
    // `VALUES ?activeTheme { <iri> }`. Three scoping disciplines keep resolution
    // O(theme-graph) regardless of Swarm size (see the .spin.ttl header for the
    // failure this prevents):
    //   1. The theme-definition walk lives in GRAPH <urn:design:theme>.
    //   2. ?activeTheme is bound to a constant (not joined cross-graph).
    //   3. Token properties are emitted via `?tProp a rdf:Property ; ?token
    //      ?tProp ?tVal` — the predicate is BOUND (to declared design
    //      properties), never the catch-all `?token ?tProp ?tVal`, whose
    //      unbound predicate defeats Oxigraph's GRAPH scoping and scans the
    //      default graph.
    let active = match active_theme_iri(store)? {
        Some(iri) => iri,
        None => return Ok(Vec::new()),
    };
    // Performance note: the CONSTRUCTs run directly against `store`. The cost is
    // dominated by the `design:extends*` property-path walk over all loaded
    // themes — ~1.4s in a release build, ~13s in a debug/unoptimized build —
    // and is essentially INDEPENDENT of the store backend (in-memory ≈ RocksDB;
    // see the `resolve_in_memory_vs_rocksdb` probe). The embedded Station builds
    // antenna unoptimized for Flutter's debug config, so `flutter run` shows a
    // ~13s themed-gate delay; `flutter run --release` boots in ~1.4s. An earlier
    // attempt to mirror THEME_GRAPH into an in-memory store was reverted because
    // it gave no real speedup once measured on equal footing (the original "14s
    // RocksDB vs 1s in-memory" was a debug-vs-release comparison, not a backend
    // one). The named-graph isolation in `theme_resolver.spin.ttl` is what keeps
    // this O(theme-graph) rather than O(whole-store).
    let mut out = Vec::new();
    for q_iri in RESOLVER_QUERIES {
        let text = fetch_query_text(store, q_iri)?;
        run_construct(store, &bind_active_theme(&text, &active), &mut out)?;
    }
    Ok(out)
}

/// Resolve the active theme IRI in `<iri>` display form. A `radio:hasTheme`
/// override (default graph) wins; otherwise the `design:active true` fallback
/// (theme graph) applies. Returns None when neither selects a theme — the
/// caller then yields an empty bundle (Station boots black per Decision B2).
fn active_theme_iri(store: &RdfStore) -> Result<Option<String>> {
    let radio = "PREFIX radio: <http://resonator.network/v2/radio#> \
                 SELECT ?t WHERE { ?_r radio:hasTheme ?t } LIMIT 1";
    if let Some(iri) = first_binding(store, radio, "t")? {
        return Ok(Some(iri));
    }
    let active = format!(
        "PREFIX design: <{DESIGN_NS}> \
         SELECT ?t WHERE {{ GRAPH <{THEME_GRAPH}> {{ ?t a design:Theme ; design:active true }} }} \
         LIMIT 1"
    );
    first_binding(store, &active, "t")
}

/// Run a SELECT and return the first solution's `var` binding in term display
/// form (`<iri>` / `"literal"`), or None if there are no solutions.
fn first_binding(store: &RdfStore, sparql: &str, var: &str) -> Result<Option<String>> {
    if let QueryResults::Solutions(mut solutions) = store.query(sparql)? {
        if let Some(sol) = solutions.next() {
            if let Some(term) = sol?.get(var) {
                return Ok(Some(term.to_string()));
            }
        }
    }
    Ok(None)
}

/// Inject `VALUES ?activeTheme { <iri> }` immediately after the query's top
/// `WHERE {` so the theme walk runs with the active theme pinned to a constant.
/// `iri_display` is already in term form (`<…>`), as produced by Oxigraph's
/// `Term::to_string`.
fn bind_active_theme(query: &str, iri_display: &str) -> String {
    query.replacen(
        "WHERE {",
        &format!("WHERE {{ VALUES ?activeTheme {{ {iri_display} }}"),
        1,
    )
}

/// Look up the `sp:text` literal of a resolver query by its IRI. Scoped to
/// THEME_GRAPH (where [`load_resolver`] puts it) so a stale resolver version
/// left in the default graph by an older build can't be picked up.
fn fetch_query_text(store: &RdfStore, query_iri: &str) -> Result<String> {
    let select = format!(
        "PREFIX sp: <http://spinrdf.org/sp#> \
         SELECT ?text WHERE {{ GRAPH <{THEME_GRAPH}> {{ <{query_iri}> sp:text ?text }} }}"
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

/// Number of role bindings voidline declares: 54 color roles + 15 type
/// roles. (Press-and-hold overlay added 7 colour roles in the messenger
/// reactions rewrite — overlayScrim, reactionBarBg, reactionBarBorder,
/// menuSurface, menuDivider, menuDestructiveBg, menuDestructiveFg.)
/// Exposed for tests and for callers wanting to sanity-check
/// completeness; not load-bearing in the resolver itself.
#[doc(hidden)]
pub const VOIDLINE_ROLE_COUNT: usize = 69;

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

    /// Load vocab, every shipped theme, and the resolver into `store`'s
    /// THEME_GRAPH. Backend-agnostic so both the in-memory test store and the
    /// RocksDB perf probe populate identically.
    fn populate_themes(store: &RdfStore) {
        let bundles: Vec<String> = std::iter::once("arch/ontology/design.ttl".to_string())
            .chain(std::iter::once(
                "themes/voidline/voidline.ttl".to_string(),
            ))
            .chain(std::iter::once(
                "themes/voidline-cb-safe/voidline-cb-safe.ttl".to_string(),
            ))
            .chain(
                TERMINAL_THEMES
                    .iter()
                    .map(|(dir, _local)| format!("themes/{dir}/{dir}.ttl")),
            )
            .collect();
        for path in &bundles {
            let ttl = std::fs::read_to_string(rel(path)).expect("read theme file");
            store
                .insert_turtle_to_graph(&ttl, THEME_GRAPH)
                .expect("insert theme into theme graph");
        }
        load_resolver(store, &rel("antenna/spin/theme_resolver.spin.ttl"))
            .expect("load resolver");
    }

    /// Build a fresh in-memory store with vocab, every shipped theme, and
    /// the resolver loaded.
    fn build_store() -> RdfStore {
        let store = RdfStore::open(None).expect("in-memory store");
        populate_themes(&store);
        store
    }

    /// Terminal-derived themes shipped alongside voidline. Tuple is
    /// (directory under `themes/` == file stem, local IRI name). Tested for
    /// role completeness en masse below so a typo in any new bundle surfaces
    /// in CI.
    const TERMINAL_THEMES: &[(&str, &str)] = &[
        ("tokyo-night",      "tokyoNight"),
        ("tokyo-night-day",  "tokyoNightDay"),
        ("catppuccin-mocha", "catppuccinMocha"),
        ("catppuccin-latte", "catppuccinLatte"),
        ("dracula",          "dracula"),
        ("dracula-light",    "draculaLight"),
        ("nord",             "nord"),
        ("nord-light",       "nordLight"),
        ("rose-pine",        "rosePine"),
        ("rose-pine-dawn",   "rosePineDawn"),
    ];

    fn activate(store: &RdfStore, theme_iri: &str) {
        // design:active lives in the theme graph (THEME_GRAPH), so the toggle
        // must be graph-scoped or the resolver — which reads that graph —
        // won't observe it.
        let upd = format!(
            "PREFIX design: <{DESIGN_NS}>
             DELETE {{ GRAPH <{THEME_GRAPH}> {{ ?t design:active true }} }}
             INSERT {{ GRAPH <{THEME_GRAPH}> {{ <{theme_iri}> design:active true }} }}
             WHERE  {{ GRAPH <{THEME_GRAPH}> {{ ?t a design:Theme . OPTIONAL {{ ?t design:active true }} }} }}"
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
        // default in the source ttl, so we explicitly clear it. The flag lives
        // in THEME_GRAPH, so the clear must be graph-scoped.
        store
            .update(&format!(
                "PREFIX design: <{DESIGN_NS}>
                 DELETE {{ GRAPH <{THEME_GRAPH}> {{ ?t design:active true }} }}
                 WHERE  {{ GRAPH <{THEME_GRAPH}> {{ ?t design:active true }} }}"
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
    fn resolver_ignores_lookalike_triples_in_default_graph() {
        // Regression guard for the embedded-messenger2 black-boot hang. The
        // embedded path loads the entire Swarm (hundreds of conversation /
        // contact triples) into the DEFAULT graph, where the resolver's theme
        // walk used to run. That made resolution O(whole-store): on a hydrated
        // Swarm the worker pinned ~100% CPU and never returned, so the theme
        // bundle never emitted and Station hung on a black B2 boot gate.
        //
        // Theme data now lives in THEME_GRAPH and the resolver is scoped to it.
        // We prove the scoping two ways at once: a default-graph triple shaped
        // exactly like a role binding hung off the active theme must NOT leak
        // into the bundle (a scoped resolver cannot see default-graph data, so
        // it also cannot fan out across the Swarm).
        let store = build_store();
        activate(&store, &format!("{VOIDLINE_NS}voidline"));

        store
            .insert_turtle(&format!(
                "<{VOIDLINE_NS}voidline> <{DESIGN_NS}bindsRole> <urn:swarm:fakeBinding> .
                 <urn:swarm:fakeBinding> <{DESIGN_NS}role> <{DESIGN_NS}fakeRole> ;
                                         <{DESIGN_NS}to>   <urn:swarm:fakeToken> .
                 <urn:swarm:fakeToken> <{DESIGN_NS}hex> \"#FF00FF\" ."
            ))
            .expect("insert look-alike default-graph triples");

        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        assert!(
            !roles.contains_key(&iri(DESIGN_NS, "fakeRole")),
            "resolver leaked a default-graph look-alike binding into the bundle",
        );
        assert_eq!(
            roles.len(),
            VOIDLINE_ROLE_COUNT,
            "canonical voidline bundle must be unchanged by default-graph noise",
        );
    }

    #[test]
    fn resolver_cost_is_independent_of_default_graph_size() {
        // The exact failure that hung embedded messenger2: a hydrated Swarm
        // loads thousands of triples into the DEFAULT graph, and the (pre-fix)
        // resolver scanned them, degrading to a near-cartesian walk that
        // pinned the worker at ~100% CPU and never emitted ThemeBundleComplete.
        // design:active selection path. Assert correctness AND that resolve
        // cost is independent of default-graph size (clean vs bloated ratio,
        // robust against CPU contention from parallel tests).
        let bloated = {
            let s = build_store();
            activate(&s, &format!("{VOIDLINE_NS}voidline"));
            let mut bulk = String::with_capacity(8000 * 96);
            for i in 0..8000 {
                bulk.push_str(&format!(
                    "<urn:swarm:msg:{i}> a <urn:msg:Message> ; \
                     <urn:msg:body> \"hello {i}\" ; \
                     <urn:msg:author> <urn:swarm:peer:{}> .\n",
                    i % 50
                ));
            }
            s.insert_turtle(&bulk).expect("insert swarm-scale bulk");
            s
        };
        let clean = {
            let s = build_store();
            activate(&s, &format!("{VOIDLINE_NS}voidline"));
            s
        };

        let t0 = std::time::Instant::now();
        let triples = resolve_active_theme(&bloated).expect("resolve");
        let bloated_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        resolve_active_theme(&clean).expect("resolve clean");
        let clean_ms = t1.elapsed().as_millis();

        assert_eq!(
            role_map(&triples).len(),
            VOIDLINE_ROLE_COUNT,
            "bundle must be correct despite 8k default-graph triples",
        );
        assert!(
            bloated_ms <= clean_ms * 4 + 500,
            "resolve scaled with default-graph size: clean={clean_ms}ms \
             bloated={bloated_ms}ms — resolver is scanning the default graph",
        );
    }

    #[test]
    fn resolver_radio_has_theme_cost_independent_of_default_graph() {
        // The EXACT messenger2 path: theme selected via radio:hasTheme (branch
        // 1), default graph bloated with a hydrated Swarm. This is the case
        // that hung live. Assert the bundle is correct AND that resolve cost
        // does not scale with default-graph size (compare clean vs bloated
        // rather than an absolute wall-clock bound, which flakes under parallel
        // test CPU load).
        let bloated = {
            let s = build_store();
            set_radio_theme(&s, &format!("{VOIDLINE_NS}voidline"));
            let mut bulk = String::with_capacity(8000 * 110);
            for i in 0..8000 {
                bulk.push_str(&format!(
                    "<urn:swarm:msg:{i}> a <urn:msg:Message> ; \
                     <urn:msg:body> \"hello {i}\" ; \
                     <urn:msg:author> <urn:swarm:peer:{}> ; \
                     <urn:msg:conv> <urn:swarm:conv:{}> .\n",
                    i % 50,
                    i % 7
                ));
            }
            s.insert_turtle(&bulk).expect("insert swarm-scale bulk");
            s
        };
        let clean = {
            let s = build_store();
            set_radio_theme(&s, &format!("{VOIDLINE_NS}voidline"));
            s
        };

        let t0 = std::time::Instant::now();
        let triples = resolve_active_theme(&bloated).expect("resolve");
        let bloated_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        resolve_active_theme(&clean).expect("resolve clean");
        let clean_ms = t1.elapsed().as_millis();

        assert_eq!(
            role_map(&triples).len(),
            VOIDLINE_ROLE_COUNT,
            "radio:hasTheme bundle correct despite 8k default-graph triples",
        );
        // Non-scaling: 8k extra default-graph triples must not blow up resolve.
        // Pre-fix this was ~700x (12ms → 8s); allow generous noise headroom.
        assert!(
            bloated_ms <= clean_ms * 4 + 500,
            "resolve scaled with default-graph size: clean={clean_ms}ms \
             bloated={bloated_ms}ms — branch-1 path is scanning the Swarm",
        );
    }

    #[test]
    fn missing_resolver_query_errors_clearly() {
        let store = RdfStore::open(None).expect("store");
        // An active theme is selected (so resolution proceeds past the
        // active-theme lookup) but no resolver queries are loaded. The missing
        // query must surface as a clear, IRI-naming error rather than a silent
        // empty bundle.
        set_radio_theme(&store, &format!("{VOIDLINE_NS}voidline"));
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

    // ---- per-radio override (Decisions VV + WW) -----------------------------

    const RADIO_NS: &str = "http://resonator.network/v2/radio#";

    /// Insert a `<urn:radio:self> radio:hasTheme <theme>` triple. Mirrors what
    /// a radio's seed.ttl declares for per-radio scoping.
    fn set_radio_theme(store: &RdfStore, theme_iri: &str) {
        let upd = format!(
            "PREFIX radio: <{RADIO_NS}>
             INSERT DATA {{ <urn:radio:self> radio:hasTheme <{theme_iri}> }}"
        );
        store.update(&upd).expect("set radio:hasTheme");
    }

    #[test]
    fn radio_has_theme_overrides_design_active() {
        // Voidline is design:active true (set by build_store via the .ttl
        // bundle). Pin :voidlineCbSafe via radio:hasTheme — the override
        // branch must win.
        let store = build_store();
        set_radio_theme(&store, &format!("{CB_NS}voidlineCbSafe"));
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        assert_eq!(
            roles.get(&iri(DESIGN_NS, "structural")).map(String::as_str),
            Some(iri(CB_NS, "pulseMagentaCb").as_str()),
            "radio:hasTheme override must select cb-safe's structural binding",
        );
    }

    #[test]
    fn no_radio_has_theme_falls_back_to_design_active() {
        // No radio:hasTheme triple — resolver falls back to :voidline (the
        // theme carrying design:active true in the .ttl bundle).
        let store = build_store();
        let triples = resolve_active_theme(&store).expect("resolve");
        let roles = role_map(&triples);

        assert_eq!(
            roles.get(&iri(DESIGN_NS, "structural")).map(String::as_str),
            Some(iri(VOIDLINE_NS, "pulseMagenta").as_str()),
            "without radio:hasTheme, fallback must pick :voidline",
        );
    }

    #[test]
    fn radio_has_theme_unresolved_uri_yields_empty_graph() {
        // radio:hasTheme points at a URI that has no design:Theme triples —
        // resolver emits empty (Decision WW). The dispatch layer surfaces a
        // WARN log on top of that; the resolver itself is silent.
        let store = build_store();
        set_radio_theme(&store, "urn:no-such-theme");
        let triples = resolve_active_theme(&store).expect("resolve");

        assert!(
            triples.is_empty(),
            "unresolved radio:hasTheme URI yields empty graph; got {} triples",
            triples.len(),
        );
    }

    // ---- terminal-derived themes -------------------------------------------

    /// Each terminal-derived bundle must bind every role voidline declares.
    /// This is the parametric guard that catches a missed binding in any new
    /// theme: the role count must equal `VOIDLINE_ROLE_COUNT` (54 colour
    /// roles + 15 type roles, all inherited via design:extends voidline plus
    /// a wholesale colour-role rebind).
    #[test]
    fn terminal_themes_bind_every_role() {
        let store = build_store();
        for (dir, local) in TERMINAL_THEMES {
            let ns = format!("http://resonator.network/v2/themes/{dir}#");
            let theme_iri = format!("{ns}{local}");
            activate(&store, &theme_iri);
            let triples =
                resolve_active_theme(&store).expect(&format!("resolve {local}"));
            let roles = role_map(&triples);
            assert_eq!(
                roles.len(),
                VOIDLINE_ROLE_COUNT,
                "{local} must bind {VOIDLINE_ROLE_COUNT} roles (54 colour + 15 type via voidline inheritance); got {}",
                roles.len(),
            );
        }
    }

    /// What goes over the wire after a theme swap, line-by-line. The
    /// `{subject} {predicate} {object} .` serialization is exactly what
    /// dispatch.rs's handle_design emits — this dumps it for a single theme
    /// so we can eyeball whether icon name + svgPath chain by blank-node
    /// label or not (Station's ThemeStore depends on that chaining).
    #[test]
    #[ignore]
    fn dump_resolved_wire_format_for_tokyo_night() {
        let store = build_store();
        let theme_iri = "http://resonator.network/v2/themes/tokyo-night#tokyoNight";
        activate(&store, theme_iri);
        let triples = resolve_active_theme(&store).expect("resolve");
        for t in triples
            .iter()
            .filter(|t| {
                t.predicate.to_string().contains("name")
                    || t.predicate.to_string().contains("svgPath")
            })
            .take(15)
        {
            eprintln!("{} {} {} .", t.subject, t.predicate, t.object);
        }
    }

    /// Icons declared via design:hasToken on voidline must reach extending
    /// themes through the SPIN resolver's `design:extends*` walk *and* the
    /// triples for each icon must chain by blank-node label, so Station's
    /// ThemeStore can accumulate `design:name` + `design:svgPath` into the
    /// same _PendingToken. Without this guard, switching to e.g. Tokyo Night
    /// would leave StationIcon asserting on every glyph (`theme has no icon
    /// "copy"`).
    #[test]
    fn terminal_themes_inherit_voidline_icons() {
        let store = build_store();
        for (dir, local) in TERMINAL_THEMES {
            let ns = format!("http://resonator.network/v2/themes/{dir}#");
            let theme_iri = format!("{ns}{local}");
            activate(&store, &theme_iri);
            let triples =
                resolve_active_theme(&store).expect(&format!("resolve {local}"));

            let svg_pred = format!("<{DESIGN_NS}svgPath>");
            let name_pred = format!("<{DESIGN_NS}name>");

            // Collect (subject -> name) and (subject -> svgPath) maps so we
            // can verify the triples for one icon share a subject.
            let mut names: HashMap<String, String> = HashMap::new();
            let mut svgs: HashMap<String, String> = HashMap::new();
            for t in &triples {
                let p = t.predicate.to_string();
                if p == name_pred {
                    names.insert(t.subject.to_string(), t.object.to_string());
                }
                if p == svg_pred {
                    svgs.insert(t.subject.to_string(), t.object.to_string());
                }
            }

            // Pair them up: every icon needs both a name and an svgPath
            // emitted under the same subject term.
            let mut copy_seen = false;
            for (subject, raw_name) in &names {
                let stripped = raw_name.trim_matches('"');
                if stripped == "copy" && svgs.contains_key(subject) {
                    copy_seen = true;
                }
            }
            assert!(
                copy_seen,
                "{local}: an icon named \"copy\" must resolve with both design:name and design:svgPath chained by blank-node label",
            );
        }
    }

    /// Driving the picker swaps `radio:hasTheme` rather than flipping
    /// `design:active true`. This regression test exercises the exact wire
    /// format the picker emits — it verifies the SPARQL update lands the
    /// new IRI and the next resolve returns the inherited icons under it.
    /// Without this guard, the live messenger crash (`StationIcon: theme
    /// has no icon "copy"`) wouldn't be caught by tests that activate via
    /// the design:active path instead.
    #[test]
    fn radio_has_theme_swap_resolves_inherited_icons() {
        let store = build_store();
        // Mimic the messenger seed: radio starts on voidline.
        set_radio_theme(&store, "http://resonator.network/v2/themes/voidline#voidline");

        // The exact SPARQL the Station picker emits, parametrised on the
        // target theme. Mirrors station/lib/ui/components/theme_picker.dart.
        let target = "http://resonator.network/v2/themes/tokyo-night#tokyoNight";
        let radio_pred = "<http://resonator.network/v2/radio#hasTheme>";
        let upd = format!(
            "DELETE {{ <urn:radio:self> {radio_pred} ?old }} \
             INSERT {{ <urn:radio:self> {radio_pred} <{target}> }} \
             WHERE  {{ OPTIONAL {{ <urn:radio:self> {radio_pred} ?old }} }}"
        );
        store.update(&upd).expect("apply picker swap");

        let triples = resolve_active_theme(&store).expect("resolve after swap");
        let svg_pred = format!("<{DESIGN_NS}svgPath>");
        let name_pred = format!("<{DESIGN_NS}name>");
        let mut copy_seen = false;
        for t in &triples {
            if t.predicate.to_string() == name_pred
                && t.object.to_string().trim_matches('"') == "copy"
            {
                let subject = t.subject.to_string();
                if triples
                    .iter()
                    .any(|u| u.subject.to_string() == subject && u.predicate.to_string() == svg_pred)
                {
                    copy_seen = true;
                    break;
                }
            }
        }
        assert!(
            copy_seen,
            "post-swap resolve must still emit the \"copy\" icon (name + svgPath chained)",
        );

        // And the canvas role must come from tokyoNight's own namespace —
        // proving the swap actually flipped, not just that voidline's data
        // was read.
        let roles = role_map(&triples);
        let canvas = roles
            .get(&iri(DESIGN_NS, "canvas"))
            .expect("canvas role bound after swap");
        assert!(
            canvas.contains("tokyo-night"),
            "post-swap canvas must resolve to a tokyo-night token; got {canvas}",
        );
    }

    /// Each terminal-derived bundle's canvas binding must come from its OWN
    /// namespace, not voidline's. Catches the easy mistake of pasting a
    /// binding that points at `voidline:voidLine` instead of the local token.
    #[test]
    fn terminal_themes_bind_canvas_from_own_namespace() {
        let store = build_store();
        for (dir, local) in TERMINAL_THEMES {
            let ns = format!("http://resonator.network/v2/themes/{dir}#");
            let theme_iri = format!("{ns}{local}");
            activate(&store, &theme_iri);
            let triples =
                resolve_active_theme(&store).expect(&format!("resolve {local}"));
            let roles = role_map(&triples);
            let canvas = roles
                .get(&iri(DESIGN_NS, "canvas"))
                .unwrap_or_else(|| panic!("{local}: canvas role unbound"));
            assert!(
                canvas.contains(dir),
                "{local}: canvas should resolve to a token in own namespace, got {canvas}",
            );
        }
    }

    // ---- perf probe (manual; #[ignore]) ------------------------------------

    /// Guards against re-misdiagnosing resolver latency. Times the canonical
    /// `resolve_active_theme` on identical 10-theme data across two backends —
    /// an in-memory store and a RocksDB store — and reports whether the build is
    /// optimized.
    ///
    /// The lesson it preserves: resolve cost is dominated by the
    /// `design:extends*` walk and is ~equal across backends (in-memory ≈
    /// RocksDB). What swings it ~10x is the BUILD PROFILE: ≈1.4s release vs ≈13s
    /// debug. The original "14s RocksDB vs 1s in-memory" was a debug-vs-release
    /// comparison, NOT a backend one — so do NOT reintroduce an in-memory mirror
    /// on that false premise. The embedded Station builds antenna unoptimized for
    /// Flutter's debug config (hence the slow `flutter run` boot); `--release`
    /// boots fast. Named-graph isolation (theme_resolver.spin.ttl) is the real
    /// scaling fix — it keeps this O(theme-graph), not O(whole-store).
    ///
    /// Run optimized to mirror the shipped app:
    ///   cargo test --release --lib resolve_in_memory_vs_rocksdb \
    ///       -- --ignored --nocapture
    /// #[ignore]'d — wall-clock, not a CI gate (see existing perf-test notes on
    /// flakiness under parallel load).
    #[test]
    #[ignore]
    fn resolve_in_memory_vs_rocksdb() {
        use std::time::Instant;
        fn timed(f: impl FnOnce() -> usize) -> (u128, usize) {
            let t = Instant::now();
            let n = f();
            (t.elapsed().as_millis(), n)
        }

        let mem = build_store();
        activate(&mem, &format!("{VOIDLINE_NS}voidline"));

        let dir = std::env::temp_dir().join(format!("antenna-theme-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rocks = RdfStore::open(Some(dir.to_str().unwrap())).expect("open rocksdb store");
        populate_themes(&rocks);
        activate(&rocks, &format!("{VOIDLINE_NS}voidline"));

        let (mem_ms, mem_n) = timed(|| resolve_active_theme(&mem).unwrap().len());
        let (rocks_ms, rocks_n) = timed(|| resolve_active_theme(&rocks).unwrap().len());

        eprintln!("\n=== RESOLVE PERF PROBE (optimized={}) ===", !cfg!(debug_assertions));
        eprintln!("  in-memory backend: {mem_ms:>6} ms  ({mem_n} triples)");
        eprintln!("  RocksDB backend:   {rocks_ms:>6} ms  ({rocks_n} triples)");
        eprintln!("  -> backends ~equal; build profile (release vs debug) is the ~10x lever.");
        eprintln!("===========================================\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
