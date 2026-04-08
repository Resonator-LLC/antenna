pub mod carrier_tox;
pub mod channel;
pub mod dag;
pub mod dispatch;
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
        eprintln!("antenna: store opened");

        // Load pipeline/DAG definition if provided
        if let Some(path) = pipeline_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            eprintln!("antenna: loaded pipeline from {}", path);
        }

        // Load seed data before building DAG so ScriptNode definitions are visible
        if let Some(path) = seed_path {
            let ttl = std::fs::read_to_string(path)?;
            store.insert_turtle(&ttl)?;
            eprintln!("antenna: loaded seed data from {}", path);
        }

        // Build the script DAG from the store
        let dag = Dag::load(&store)?;

        // Channel for carrier events (Turtle strings from C callback)
        let (event_tx, event_rx) = mpsc::channel::<String>();

        // Create carrier (Tox)
        let tox = ToxCarrier::new(profile, nodes_path, event_tx)?;
        eprintln!("antenna: tox carrier started");

        Ok(Self {
            store,
            dag,
            tox,
            tox_event_rx: event_rx,
        })
    }

    /// One tick: iterate carrier, drain events to OUT, drain IN and dispatch.
    pub fn tick(
        &mut self,
        input: &mut dyn AntennaIn,
        output: &mut dyn AntennaOut,
    ) -> Result<()> {
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
                eprintln!("antenna: insert error: {}", e);
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
                eprintln!("antenna: script emit insert: {}", e);
            }
            output.send(turtle);
        }

        Ok(())
    }

    /// Run the event loop. Blocks until shutdown.
    /// Uses poll() on the IN clock fd with carrier interval as timeout.
    pub fn run(
        &mut self,
        input: &mut dyn AntennaIn,
        output: &mut dyn AntennaOut,
    ) -> Result<()> {
        loop {
            let timeout_ms = self.tox.iteration_interval().as_millis() as i32;

            if let Some(clock_fd) = input.clock_fd() {
                let mut pfd = libc::pollfd {
                    fd: clock_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
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
