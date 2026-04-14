// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Antenna — RDF stream processor with P2P networking, scripting, and SPARQL store.
//!
//! Antenna receives RDF Turtle on its input, dispatches by `rdf:type` (SPIN queries,
//! Tox commands, or raw data), routes data through a scriptable DAG, stores everything
//! in an embedded Oxigraph store, and emits results as Turtle on its output.
//!
//! Transports are trait-based (`AntennaIn`/`AntennaOut`): stdin/stdout pipes, WebSocket,
//! or lock-free ring buffer channels for FFI embedding.

pub mod carrier_tox;
pub mod channel;
pub mod dag;
pub mod dispatch;
pub mod fd_limit;
pub mod llm;
pub mod script_vm;
pub mod store;
pub mod ws;

use anyhow::Result;
use std::sync::mpsc;
use std::time::Duration;

use crate::carrier_tox::ToxCarrier;
use crate::channel::{AntennaIn, AntennaOut};
use crate::dag::Dag;
use crate::store::RdfStore;

pub struct AntennaContext {
    pub store: RdfStore,
    pub dag: Dag,
    pub tox: ToxCarrier,
    tox_event_rx: mpsc::Receiver<String>,
}

impl AntennaContext {
    pub fn new(
        profile: &str,
        store_path: Option<&str>,
        pipeline_path: Option<&str>,
        nodes_path: Option<&str>,
        seed_path: Option<&str>,
    ) -> Result<Self> {
        let store = RdfStore::open(store_path)?;
        tracing::info!("store opened");

        // Load pipeline/DAG definition if provided
        if let Some(path) = pipeline_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            tracing::info!(path, "loaded pipeline");
        }

        // Load seed data before building DAG so ScriptNode definitions are visible
        if let Some(path) = seed_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            tracing::info!(path, "loaded seed data");
        }

        // Build the script DAG from the store
        let dag = Dag::load(&store)?;

        // Channel for carrier events (Turtle strings from C callback)
        let (event_tx, event_rx) = mpsc::channel::<String>();

        // Create carrier (Tox)
        let tox = ToxCarrier::new(profile, nodes_path, event_tx)?;
        tracing::info!("tox carrier started");

        Ok(Self {
            store,
            dag,
            tox,
            tox_event_rx: event_rx,
        })
    }

    /// One tick: iterate carrier, drain events to OUT, drain IN and dispatch.
    pub fn tick(&mut self, input: &mut dyn AntennaIn, output: &mut dyn AntennaOut) -> Result<()> {
        // 0. Clock tick — wake clock-driven scripts
        self.dag.broadcast(
            "http://resonator.network/v2/antenna#clock",
            "[] a <http://resonator.network/v2/antenna#ClockTick> .",
        );

        // 1. Poll carrier for Tox events
        self.tox.iterate()?;

        // 2. Drain carrier events → OUT
        while let Ok(turtle) = self.tox_event_rx.try_recv() {
            output.send(&turtle);
            // Also route through DAG channels (carrier events go to beforeInsert)
            self.dag.before_insert(&turtle);
            // Insert into store
            if let Err(e) = self.store.insert_turtle(&turtle) {
                tracing::warn!(%e, "insert error");
            }
            self.dag.after_insert(&turtle);
        }

        // 3. Drain IN → dispatch
        while let Some(line) = input.recv() {
            if line.is_empty() {
                continue;
            }
            dispatch::dispatch(&line, &self.store, &self.dag, &self.tox, output);
        }

        // 4. Process store queries from script threads
        self.dag.pump_queries(&self.store);

        // 5. Pump script emit outputs — insert into store and send to WS clients
        let emits = self.dag.pump_emits();
        for turtle in &emits {
            if let Err(e) = self.store.insert_turtle(turtle) {
                tracing::warn!(%e, "script emit insert error");
            }
            output.send(turtle);
        }

        // 6. Check for dead node threads
        let dead = self.dag.health_check();
        if !dead.is_empty() {
            tracing::error!(nodes = ?dead, "DAG nodes have crashed");
        }

        Ok(())
    }

    /// Run the event loop. Blocks until shutdown.
    /// Uses poll() on the IN clock fd with carrier interval as timeout.
    pub fn run(&mut self, input: &mut dyn AntennaIn, output: &mut dyn AntennaOut) -> Result<()> {
        loop {
            let timeout_ms = self.tox.iteration_interval().as_millis() as i32;

            if let Some(clock_fd) = input.clock_fd() {
                let mut pfd = libc::pollfd {
                    fd: clock_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: pfd is a valid stack-allocated pollfd; clock_fd is a
                // valid fd owned by the input transport. poll() blocks until
                // data arrives or timeout_ms elapses.
                unsafe {
                    libc::poll(&mut pfd, 1, timeout_ms);
                }
            } else {
                std::thread::sleep(Duration::from_millis(timeout_ms as u64));
            }

            self.tick(input, output)?;
        }
    }

    /// Carrier iteration interval hint.
    pub fn interval(&self) -> Duration {
        self.tox.iteration_interval()
    }
}

/// Builder for `AntennaContext`.
pub struct AntennaBuilder {
    profile: String,
    store_path: Option<String>,
    pipeline_path: Option<String>,
    nodes_path: Option<String>,
    seed_path: Option<String>,
}

impl AntennaBuilder {
    pub fn new(profile: &str) -> Self {
        Self {
            profile: profile.to_string(),
            store_path: None,
            pipeline_path: None,
            nodes_path: None,
            seed_path: None,
        }
    }

    pub fn store_path(mut self, path: &str) -> Self {
        self.store_path = Some(path.to_string());
        self
    }

    pub fn pipeline(mut self, path: &str) -> Self {
        self.pipeline_path = Some(path.to_string());
        self
    }

    pub fn nodes(mut self, path: &str) -> Self {
        self.nodes_path = Some(path.to_string());
        self
    }

    pub fn seed(mut self, path: &str) -> Self {
        self.seed_path = Some(path.to_string());
        self
    }

    pub fn build(self) -> Result<AntennaContext> {
        AntennaContext::new(
            &self.profile,
            self.store_path.as_deref(),
            self.pipeline_path.as_deref(),
            self.nodes_path.as_deref(),
            self.seed_path.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chains_all_options() {
        // Verify the builder compiles and chains without panic.
        // We can't call build() without a valid Tox profile, but we can
        // verify the fluent API works.
        let builder = AntennaBuilder::new("/tmp/test.tox")
            .store_path("/tmp/store")
            .pipeline("pipeline.ttl")
            .nodes("nodes.json")
            .seed("seed.ttl");
        assert_eq!(builder.profile, "/tmp/test.tox");
        assert_eq!(builder.store_path.as_deref(), Some("/tmp/store"));
        assert_eq!(builder.pipeline_path.as_deref(), Some("pipeline.ttl"));
        assert_eq!(builder.nodes_path.as_deref(), Some("nodes.json"));
        assert_eq!(builder.seed_path.as_deref(), Some("seed.ttl"));
    }

    #[test]
    fn builder_defaults_are_none() {
        let builder = AntennaBuilder::new("profile.tox");
        assert!(builder.store_path.is_none());
        assert!(builder.pipeline_path.is_none());
        assert!(builder.nodes_path.is_none());
        assert!(builder.seed_path.is_none());
    }
}
