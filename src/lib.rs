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
pub mod llm;
pub mod logging;
pub mod script_vm;
pub mod store;
pub mod theme;
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
/// then voidline-cb-safe (extends voidline).
const DESIGN_BUNDLE: &[&str] = &[
    include_str!("../../arch/ontology/design.ttl"),
    include_str!("../../themes/voidline/voidline.ttl"),
    include_str!("../../themes/voidline-cb-safe/voidline-cb-safe.ttl"),
];

/// SPIN-encoded theme resolver — three CONSTRUCT queries the dispatch
/// handler runs against the store on `design:ResolveActiveTheme`.
const THEME_RESOLVER_TTL: &str = include_str!("../spin/theme_resolver.spin.ttl");

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
        tracing::info!(target: "DESIGN", "loaded design ontology + voidline themes + resolver");

        if let Some(path) = pipeline_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            tracing::info!(target: "PIPELINE", path, "loaded pipeline");
        }

        if let Some(path) = seed_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            tracing::info!(target: "PIPELINE", path, "loaded seed data");
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
