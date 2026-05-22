// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Antenna — RDF stream processor with P2P networking, scripting, and SPARQL store.
//!
//! Antenna receives RDF Turtle on its input, dispatches by `rdf:type` (SPIN queries,
//! Carrier commands, or raw data), routes data through a scriptable DAG, stores
//! everything in an embedded Oxigraph store, and emits results as Turtle on its
//! output.
//!
//! Transports are trait-based (`AntennaIn`/`AntennaOut`): stdin/stdout pipes,
//! WebSocket, or lock-free ring buffer channels for FFI embedding.

pub mod carrier;
pub mod channel;
pub mod dag;
pub mod dispatch;
pub mod ffi;
pub mod llm;
pub mod logging;
pub mod script_vm;
pub mod store;
pub mod theme;
#[cfg(feature = "ws")]
pub mod ws;

use anyhow::Result;
use std::sync::mpsc;
use std::time::Duration;

use crate::carrier::CarrierClient;
use crate::channel::{AntennaIn, AntennaOut};
use crate::dag::Dag;
use crate::store::RdfStore;

/// Design ontology + canonical themes embedded at compile time. Loaded on
/// every antenna boot so Station's B2 theme gate opens regardless of which
/// radio is running. Keep ordering: ontology first, then voidline (canonical),
/// then everything that extends voidline.
const DESIGN_BUNDLE: &[&str] = &[
    include_str!("../../arch/ontology/design.ttl"),
    include_str!("../../themes/voidline/voidline.ttl"),
    include_str!("../../themes/voidline-cb-safe/voidline-cb-safe.ttl"),
    include_str!("../../themes/tokyo-night/tokyo-night.ttl"),
    include_str!("../../themes/tokyo-night-day/tokyo-night-day.ttl"),
    include_str!("../../themes/catppuccin-mocha/catppuccin-mocha.ttl"),
    include_str!("../../themes/catppuccin-latte/catppuccin-latte.ttl"),
    include_str!("../../themes/dracula/dracula.ttl"),
    include_str!("../../themes/dracula-light/dracula-light.ttl"),
    include_str!("../../themes/nord/nord.ttl"),
    include_str!("../../themes/nord-light/nord-light.ttl"),
    include_str!("../../themes/rose-pine/rose-pine.ttl"),
    include_str!("../../themes/rose-pine-dawn/rose-pine-dawn.ttl"),
];

/// Emoji catalog — categorised glyph table loaded into every antenna so
/// any radio that wants the press-and-hold full picker (messenger today,
/// others later) can SPARQL-walk it without shipping its own copy. Forks
/// can layer larger catalogs by emitting more `antenna:EmojiCategory`
/// nodes at seed time.
const EMOJI_CATALOG_TTL: &str = include_str!("../../arch/ontology/emoji.ttl");

/// SPIN-encoded theme resolver — three CONSTRUCT queries the dispatch
/// handler runs against the store on `design:ResolveActiveTheme`.
const THEME_RESOLVER_TTL: &str = include_str!("../spin/theme_resolver.spin.ttl");

/// Replace the pipeline triples in the store with the contents of `ttl`.
///
/// Persistent (`--store <path>`) Oxigraph stores accumulate one
/// `<urn:…:src:main> antenna:body "…"` triple per boot when run.sh edits
/// pipeline.ttl and restarts. `query_script_nodes` (dag.rs) has no
/// `ORDER BY` and uses `or_insert_with` (first-write-wins) so a stale
/// boot's body silently wins on subsequent runs — debuggable only by
/// `rm -rf $STORE_DIR`. Wiping the type-anchored ScriptNode and
/// ScriptSource triples before the new INSERT keeps radio-authored RDF
/// (drafts, peer-cache, design bundle) intact while making
/// pipeline.ttl-as-single-source-of-truth actually hold across restarts.
///
/// Pattern (b) from the proposal in arch/improvement-suggestions.md
/// § "Test roundtrip speedups (2026-05)" #1 — type-targeted, not
/// URI-targeted, on the radio convention "one pipeline file = one source
/// of truth for all script nodes." On a fresh store both DELETEs match
/// nothing and behave as no-ops.
pub fn replace_pipeline(store: &RdfStore, ttl: &str) -> Result<()> {
    const PIPELINE_RESET_NODES: &str = "
        PREFIX antenna: <http://resonator.network/v2/antenna#>
        DELETE WHERE { ?s a antenna:ScriptNode ; ?p ?o }
    ";
    const PIPELINE_RESET_SOURCES: &str = "
        PREFIX antenna: <http://resonator.network/v2/antenna#>
        DELETE WHERE { ?s a antenna:ScriptSource ; ?p ?o }
    ";
    store.update(PIPELINE_RESET_NODES)?;
    store.update(PIPELINE_RESET_SOURCES)?;
    store.insert_turtle(ttl)
}

pub struct AntennaContext {
    pub store: RdfStore,
    pub dag: Dag,
    pub carrier: CarrierClient,
    /// Account loaded or created at startup. Empty until bootstrap completes.
    pub account_id: String,
    carrier_event_rx: mpsc::Receiver<String>,
}

impl AntennaContext {
    pub fn new(
        data_dir: &str,
        account_id: Option<&str>,
        store_path: Option<&str>,
        pipeline_path: Option<&str>,
        seed_path: Option<&str>,
    ) -> Result<Self> {
        let pipeline_ttl = pipeline_path.map(std::fs::read_to_string).transpose()?;
        let seed_ttl = seed_path.map(std::fs::read_to_string).transpose()?;
        Self::new_with_ttl(
            data_dir,
            account_id,
            store_path,
            pipeline_ttl.as_deref(),
            seed_ttl.as_deref(),
        )
    }

    /// Like [`new`] but accepts pipeline and seed Turtle as in-memory content
    /// strings rather than filesystem paths. Used by the FFI shim, which
    /// loads radio assets from the embedding app's bundle.
    pub fn new_with_ttl(
        data_dir: &str,
        account_id: Option<&str>,
        store_path: Option<&str>,
        pipeline_ttl: Option<&str>,
        seed_ttl: Option<&str>,
    ) -> Result<Self> {
        let store = RdfStore::open(store_path)?;
        tracing::info!(target: "PIPELINE", "store opened");

        // Load the design ontology, canonical themes, and the SPIN-encoded
        // theme resolver into every antenna instance so Station's B2 boot
        // gate can open without each radio having to seed its own theme.
        // The TTL is embedded at compile time so the binary is self-
        // contained — no workspace-relative paths at runtime.
        for ttl in DESIGN_BUNDLE {
            store.insert_turtle(ttl)?;
        }
        theme::load_resolver_str(&store, THEME_RESOLVER_TTL)?;
        store.insert_turtle(EMOJI_CATALOG_TTL)?;
        tracing::info!(target: "DESIGN", "loaded design ontology + voidline themes + resolver + emoji catalog");

        if let Some(ttl) = pipeline_ttl {
            replace_pipeline(&store, ttl)?;
            tracing::info!(target: "PIPELINE", bytes = ttl.len(), "loaded pipeline");
        }

        if let Some(ttl) = seed_ttl {
            store.insert_turtle(ttl)?;
            tracing::info!(target: "PIPELINE", bytes = ttl.len(), "loaded seed data");
        }

        let dag = Dag::load(&store)?;

        let (event_tx, event_rx) = mpsc::channel::<String>();

        let carrier = CarrierClient::new(data_dir, event_tx)?;
        tracing::info!(target: "JAMI", data_dir, "carrier started");

        // Bootstrap an account: load if specified, create otherwise. The
        // resulting account_id is what subsequent commands address.
        let account = match account_id {
            Some(id) => {
                carrier.load_account(id)?;
                tracing::info!(target: "JAMI", account = id, "loading account");
                id.to_string()
            }
            None => {
                let new_id = carrier.create_account(None)?;
                tracing::info!(target: "JAMI", account = %new_id, "created account");
                eprintln!("antenna: created account {new_id}");
                new_id
            }
        };

        Ok(Self {
            store,
            dag,
            carrier,
            account_id: account,
            carrier_event_rx: event_rx,
        })
    }

    /// One tick: iterate carrier, drain events to OUT, drain IN and dispatch.
    pub fn tick(&mut self, input: &mut dyn AntennaIn, output: &mut dyn AntennaOut) -> Result<()> {
        self.dag.broadcast(
            "http://resonator.network/v2/antenna#clock",
            "[] a <http://resonator.network/v2/antenna#ClockTick> .",
        );

        self.carrier.iterate()?;

        while let Ok(turtle) = self.carrier_event_rx.try_recv() {
            output.send(&turtle);
            self.dag.before_insert(&turtle);
            if let Err(e) = self.store.insert_turtle(&turtle) {
                tracing::warn!(target: "SPARQL", %e, "insert error");
            }
            self.dag.after_insert(&turtle);
        }

        while let Some(line) = input.recv() {
            if line.is_empty() {
                continue;
            }
            dispatch::dispatch(
                &line,
                &self.store,
                &self.dag,
                Some(&self.carrier),
                &self.account_id,
                output,
            );
        }

        self.dag.pump_queries(&self.store);

        let emits = self.dag.pump_emits();
        for turtle in &emits {
            dispatch::dispatch(
                turtle,
                &self.store,
                &self.dag,
                Some(&self.carrier),
                &self.account_id,
                output,
            );
        }

        let dead = self.dag.health_check();
        if !dead.is_empty() {
            tracing::error!(target: "SCRIPT", nodes = ?dead, "DAG nodes have crashed");
        }

        Ok(())
    }

    pub fn run(&mut self, input: &mut dyn AntennaIn, output: &mut dyn AntennaOut) -> Result<()> {
        // Cap the per-iteration sleep so WS-driven input and async script
        // emits aren't parked behind libjami's idle interval (which can
        // grow to ~5s when nothing is happening on the swarm). The cap is
        // an upper bound; libjami's `iteration_interval` is still honored
        // when it asks us to wake sooner.
        const MAX_SLEEP_MS: i32 = 25;
        loop {
            let timeout_ms = (self.carrier.iteration_interval().as_millis() as i32)
                .clamp(1, MAX_SLEEP_MS);

            if let Some(clock_fd) = input.clock_fd() {
                let mut pfd = libc::pollfd {
                    fd: clock_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: pfd is a valid stack-allocated pollfd; clock_fd is a
                // valid fd owned by the input transport.
                unsafe {
                    libc::poll(&mut pfd, 1, timeout_ms);
                }
            } else {
                std::thread::sleep(Duration::from_millis(timeout_ms as u64));
            }

            self.tick(input, output)?;
        }
    }

    pub fn interval(&self) -> Duration {
        self.carrier.iteration_interval()
    }
}

pub struct AntennaBuilder {
    data_dir: String,
    account_id: Option<String>,
    store_path: Option<String>,
    pipeline_path: Option<String>,
    seed_path: Option<String>,
}

impl AntennaBuilder {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
            account_id: None,
            store_path: None,
            pipeline_path: None,
            seed_path: None,
        }
    }

    pub fn account(mut self, id: &str) -> Self {
        self.account_id = Some(id.to_string());
        self
    }

    pub fn store_path(mut self, path: &str) -> Self {
        self.store_path = Some(path.to_string());
        self
    }

    pub fn pipeline(mut self, path: &str) -> Self {
        self.pipeline_path = Some(path.to_string());
        self
    }

    pub fn seed(mut self, path: &str) -> Self {
        self.seed_path = Some(path.to_string());
        self
    }

    pub fn build(self) -> Result<AntennaContext> {
        AntennaContext::new(
            &self.data_dir,
            self.account_id.as_deref(),
            self.store_path.as_deref(),
            self.pipeline_path.as_deref(),
            self.seed_path.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chains_all_options() {
        let builder = AntennaBuilder::new("/tmp/jami-data")
            .account("abc123")
            .store_path("/tmp/store")
            .pipeline("pipeline.ttl")
            .seed("seed.ttl");
        assert_eq!(builder.data_dir, "/tmp/jami-data");
        assert_eq!(builder.account_id.as_deref(), Some("abc123"));
        assert_eq!(builder.store_path.as_deref(), Some("/tmp/store"));
        assert_eq!(builder.pipeline_path.as_deref(), Some("pipeline.ttl"));
        assert_eq!(builder.seed_path.as_deref(), Some("seed.ttl"));
    }

    #[test]
    fn builder_defaults_are_none() {
        let builder = AntennaBuilder::new("/tmp/jami-data");
        assert!(builder.account_id.is_none());
        assert!(builder.store_path.is_none());
        assert!(builder.pipeline_path.is_none());
        assert!(builder.seed_path.is_none());
    }

    /// Phase 3 acceptance: the antenna binary must boot with the emoji
    /// catalog pre-loaded so any radio's press-and-hold picker can walk
    /// it via store.query() without shipping its own catalog.
    #[test]
    fn emoji_catalog_loads_with_at_least_one_category_per_section() {
        let store = RdfStore::open(None).expect("in-memory store");
        store.insert_turtle(EMOJI_CATALOG_TTL).expect("insert emoji catalog");

        let results = store
            .query(
                "SELECT (COUNT(DISTINCT ?cat) AS ?n) WHERE { \
                    ?cat a <http://resonator.network/v2/antenna#EmojiCategory> \
                }",
            )
            .expect("emoji category count query");
        let mut count_str = String::new();
        if let oxigraph::sparql::QueryResults::Solutions(solutions) = results {
            for sol in solutions.flatten() {
                if let Some(term) = sol.get("n") {
                    count_str = term.to_string();
                    break;
                }
            }
        }
        assert!(
            count_str.contains("\"9\"") || count_str.contains("9"),
            "emoji catalog should declare 9 categories on a fresh boot, got {count_str}",
        );
    }

    /// Phase 1c-1 acceptance: the antenna binary must boot with the design
    /// ontology + canonical themes + resolver pre-loaded so Station's B2
    /// theme gate opens without each radio having to seed its own theme.
    /// Mirrors the load path inside [`AntennaContext::new`] without
    /// constructing a CarrierClient.
    #[test]
    fn design_bundle_and_resolver_install_into_fresh_store() {
        let store = RdfStore::open(None).expect("in-memory store");
        for ttl in DESIGN_BUNDLE {
            store.insert_turtle(ttl).expect("insert design ttl");
        }
        theme::load_resolver_str(&store, THEME_RESOLVER_TTL)
            .expect("load resolver");

        let triples = theme::resolve_active_theme(&store)
            .expect("resolver runs against pre-loaded design data");
        assert!(
            !triples.is_empty(),
            "ResolveActiveTheme must yield a non-zero bundle on a fresh \
             antenna boot — Station's B2 gate stays black otherwise",
        );
    }
}
