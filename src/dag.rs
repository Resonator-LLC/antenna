// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Channel-based script DAG. Loads ScriptNode/Channel graph from Oxigraph,
//! spawns a thread per node, routes data through internal channels.
use anyhow::Result;
use oxigraph::model::Term;
use oxigraph::sparql::QueryResults;
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use crate::channel::{ChannelReader, ChannelWriter, InternalChannel};
use crate::llm::{self, BackendChannels};
use crate::script_vm::{QueryRequest, ScriptVm};
use crate::store::RdfStore;

const ANTENNA_NS: &str = "http://resonator.network/v2/antenna#";
const DEFAULT_CHANNEL_CAPACITY: usize = 65536;
const DEFAULT_JS_MEMORY_LIMIT: usize = 32 * 1024 * 1024; // 32 MB per script

// ---------------------------------------------------------------------------
// Loaded DAG types
// ---------------------------------------------------------------------------

struct ScriptNodeDef {
    uri: String,
    body: String,
    #[allow(dead_code)]
    language: String,
    ins: Vec<String>,
    outs: Vec<String>,
    #[allow(dead_code)]
    where_clause: Option<String>,
}

struct SemanticRouterDef {
    uri: String,
    in_a: String,
    in_b: String,
    outs: Vec<String>,
    backend_type: String,
    endpoint: String,
    model: String,
    system_prompt: String,
    max_tokens: u32,
}

// ---------------------------------------------------------------------------
// Turtle validation (uses Oxigraph parser)
// ---------------------------------------------------------------------------

const TURTLE_PREFIXES: &str = "\
@prefix res: <https://resonator.network/> .\n\
@prefix antenna: <http://resonator.network/v2/antenna#> .\n\
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n";

fn validate_turtle(raw: &str) -> Result<String, String> {
    use oxigraph::io::RdfFormat;
    use oxigraph::io::RdfParser;

    // Prepend prefixes to the LLM output
    let full = format!("{}\n{}", TURTLE_PREFIXES, raw.trim());

    let parser = RdfParser::from_format(RdfFormat::Turtle);
    let mut count = 0usize;
    for result in parser.for_reader(full.as_bytes()) {
        match result {
            Ok(_) => count += 1,
            Err(e) => return Err(format!("Turtle parse error: {}", e)),
        }
    }
    if count == 0 {
        return Err("LLM produced no triples".to_string());
    }
    Ok(full)
}

// ---------------------------------------------------------------------------
// Emit router: drains emit_rx and broadcasts to OUT channels
// ---------------------------------------------------------------------------

struct EmitRoute {
    rx: mpsc::Receiver<String>,
    out_uris: Vec<String>,
}

// ---------------------------------------------------------------------------
// Running DAG
// ---------------------------------------------------------------------------

pub struct Dag {
    /// Named channels: URI → writer handles for broadcasting to subscribers
    channel_writers: HashMap<String, Vec<ChannelWriter>>,
    /// Emit routes: one per node, drained in pump_emits()
    emit_routes: Vec<EmitRoute>,
    /// Running node threads (kept alive)
    _threads: Vec<thread::JoinHandle<()>>,
    /// Track which dead nodes have been reported (avoid repeated logs)
    reported_dead: std::collections::HashSet<String>,
    /// Receiver for store query requests from script threads
    query_rx: mpsc::Receiver<QueryRequest>,
}

impl Dag {
    /// Load the DAG from Oxigraph and spawn node threads.
    pub fn load(store: &RdfStore) -> Result<Self> {
        let nodes = query_script_nodes(store)?;
        let routers = query_semantic_routers(store)?;

        // Query channel shared by all script VMs
        let (query_tx, query_rx) = mpsc::channel::<QueryRequest>();

        if nodes.is_empty() && routers.is_empty() {
            return Ok(Self {
                channel_writers: HashMap::new(),
                emit_routes: Vec::new(),
                _threads: Vec::new(),
                query_rx,
                reported_dead: std::collections::HashSet::new(),
            });
        }

        // Phase 1: Create inboxes, build broadcast map, collect pending emit routes
        let mut broadcast_map: HashMap<String, Vec<ChannelWriter>> = HashMap::new();
        let mut emit_routes = Vec::new();
        let mut threads = Vec::new();

        struct Pending {
            uri: String,
            body: String,
            inbox_readers: Vec<ChannelReader>,
            inbox_channel_uris: Vec<String>,
            emit_tx: mpsc::Sender<String>,
            emit_rx: mpsc::Receiver<String>,
            outs: Vec<String>,
        }

        let mut pending: Vec<Pending> = Vec::new();

        for node in nodes {
            let mut inbox_readers = Vec::new();
            let mut inbox_channel_uris = Vec::new();

            for in_uri in &node.ins {
                let inbox = InternalChannel::new(DEFAULT_CHANNEL_CAPACITY)?;
                inbox_readers.push(inbox.reader());

                broadcast_map
                    .entry(in_uri.clone())
                    .or_default()
                    .push(inbox.writer());

                inbox_channel_uris.push(in_uri.clone());
            }

            let (emit_tx, emit_rx) = mpsc::channel::<String>();

            pending.push(Pending {
                uri: node.uri,
                body: node.body,
                inbox_readers,
                inbox_channel_uris,
                emit_tx,
                emit_rx,
                outs: node.outs,
            });
        }

        // Phase 2: Spawn node threads, collect emit routes
        for pn in pending {
            let node_uri = pn.uri.clone();
            let node_body = pn.body;
            let inbox_readers = pn.inbox_readers;
            let inbox_channel_uris = pn.inbox_channel_uris;
            let emit_tx = pn.emit_tx;
            let node_query_tx = query_tx.clone();

            let handle = thread::Builder::new()
                .name(format!("node:{}", short_uri(&pn.uri)))
                .spawn(move || {
                    let vm = match ScriptVm::new(emit_tx, node_query_tx, DEFAULT_JS_MEMORY_LIMIT) {
                        Ok(vm) => vm,
                        Err(e) => {
                            tracing::error!(node = %node_uri, %e, "failed to create VM");
                            return;
                        }
                    };

                    loop {
                        let mut pollfds: Vec<libc::pollfd> = inbox_readers
                            .iter()
                            .map(|r| libc::pollfd {
                                fd: r.clock_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            })
                            .collect();

                        if pollfds.is_empty() {
                            break;
                        }

                        // SAFETY: pollfds is a valid Vec of pollfd structs with
                        // fds from inbox_readers (owned by this thread). poll()
                        // blocks until data arrives or 500ms timeout.
                        let n =
                            unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, 500) };
                        if n <= 0 {
                            continue;
                        }

                        for (i, pfd) in pollfds.iter().enumerate() {
                            if pfd.revents & libc::POLLIN != 0 {
                                inbox_readers[i].consume_clock();
                                while let Some(turtle) = inbox_readers[i].recv() {
                                    if turtle.is_empty() {
                                        continue;
                                    }
                                    match std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            vm.exec(
                                                &node_body,
                                                &turtle,
                                                &inbox_channel_uris[i],
                                            )
                                        }),
                                    ) {
                                        Ok(Err(e)) => {
                                            tracing::error!(
                                                node = %node_uri, %e, "script error"
                                            );
                                        }
                                        Err(panic) => {
                                            tracing::error!(
                                                node = %node_uri, ?panic, "script panicked"
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                })?;

            threads.push(handle);

            // Store the emit route for this node (always, even with no out channels)
            emit_routes.push(EmitRoute {
                rx: pn.emit_rx,
                out_uris: pn.outs,
            });
        }

        // Phase 3: Spawn SemanticRouter threads
        // Platform backend channels (shared by all platform-type routers)
        let platform_request_ch = InternalChannel::new(DEFAULT_CHANNEL_CAPACITY)?;
        let platform_response_ch = InternalChannel::new(DEFAULT_CHANNEL_CAPACITY)?;

        // Register platform channels in broadcast map so host can subscribe
        let llm_request_uri = format!("{}llmRequest", ANTENNA_NS);
        let _llm_response_uri = format!("{}llmResponse", ANTENNA_NS);
        broadcast_map
            .entry(llm_request_uri.clone())
            .or_default()
            .push(platform_request_ch.writer());

        for router in routers {
            // Create inboxes for inA and inB
            let inbox_a = InternalChannel::new(DEFAULT_CHANNEL_CAPACITY)?;
            let inbox_b = InternalChannel::new(DEFAULT_CHANNEL_CAPACITY)?;

            let reader_a = inbox_a.reader();
            let reader_b = inbox_b.reader();

            broadcast_map
                .entry(router.in_a.clone())
                .or_default()
                .push(inbox_a.writer());
            broadcast_map
                .entry(router.in_b.clone())
                .or_default()
                .push(inbox_b.writer());

            let (emit_tx, emit_rx) = mpsc::channel::<String>();

            // Create the LLM backend
            let platform_channels = if router.backend_type == "platform" {
                Some(BackendChannels {
                    request_writer: platform_request_ch.writer(),
                    response_reader: platform_response_ch.reader(),
                })
            } else {
                None
            };

            let backend = llm::create_backend(
                &router.backend_type,
                &router.endpoint,
                &router.model,
                platform_channels,
            )?;

            let node_uri = router.uri.clone();
            let system_prompt = router.system_prompt;
            let max_tokens = router.max_tokens;

            let handle = thread::Builder::new()
                .name(format!("router:{}", short_uri(&router.uri)))
                .spawn(move || {
                    let mut slot_a: Option<String> = None;
                    let mut slot_b: Option<String> = None;

                    loop {
                        let mut pollfds = [
                            libc::pollfd {
                                fd: reader_a.clock_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: reader_b.clock_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                        ];

                        // SAFETY: pollfds is a valid stack array; fds are from
                        // reader_a/reader_b owned by this thread.
                        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, 500) };
                        if n < 0 {
                            continue;
                        }

                        // Latch latest from inA
                        if pollfds[0].revents & libc::POLLIN != 0 {
                            reader_a.consume_clock();
                            while let Some(turtle) = reader_a.recv() {
                                if !turtle.is_empty() {
                                    slot_a = Some(turtle);
                                }
                            }
                        }

                        // Latch latest from inB
                        if pollfds[1].revents & libc::POLLIN != 0 {
                            reader_b.consume_clock();
                            while let Some(turtle) = reader_b.recv() {
                                if !turtle.is_empty() {
                                    slot_b = Some(turtle);
                                }
                            }
                        }

                        // Fire when both slots filled
                        if let (Some(ref a), Some(ref b)) = (&slot_a, &slot_b) {
                            let user_prompt =
                                llm::build_prompt(a, b, TURTLE_PREFIXES);

                            let system = format!(
                                "You are an RDF synthesis engine. Given two Turtle RDF graphs, \
                                 produce a third that captures the semantic relationship between them. \
                                 Output ONLY valid Turtle triples using the provided prefixes. \
                                 No explanation.\n{}",
                                system_prompt
                            );

                            match backend.complete(&system, &user_prompt, max_tokens) {
                                Ok(raw) => {
                                    // Validate and emit
                                    match validate_turtle(&raw) {
                                        Ok(valid) => {
                                            let _ = emit_tx.send(valid);
                                        }
                                        Err(err1) => {
                                            // One retry with error feedback
                                            let retry_prompt = format!(
                                                "{}\n\nYour previous output had a parse error: {}\nPlease fix and output valid Turtle only.",
                                                user_prompt, err1
                                            );
                                            match backend.complete(
                                                &system,
                                                &retry_prompt,
                                                max_tokens,
                                            ) {
                                                Ok(raw2) => match validate_turtle(&raw2) {
                                                    Ok(valid) => {
                                                        let _ = emit_tx.send(valid);
                                                    }
                                                    Err(err2) => {
                                                        let error_turtle = format!(
                                                            "@prefix antenna: <http://resonator.network/v2/antenna#> .\n\
                                                             [] a antenna:Error ; antenna:message \"{}\" .\n",
                                                            err2.replace('"', "'")
                                                        );
                                                        let _ = emit_tx.send(error_turtle);
                                                    }
                                                },
                                                Err(e) => {
                                                    tracing::warn!(node = %node_uri, %e, "LLM retry error");
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(node = %node_uri, %e, "LLM error");
                                }
                            }

                            // Clear slots after firing
                            slot_a = None;
                            slot_b = None;
                        }
                    }
                })?;

            threads.push(handle);
            emit_routes.push(EmitRoute {
                rx: emit_rx,
                out_uris: router.outs,
            });
        }

        tracing::info!(
            threads = threads.len(),
            emit_routes = emit_routes.len(),
            "DAG loaded"
        );

        Ok(Self {
            channel_writers: broadcast_map,
            emit_routes,
            _threads: threads,
            query_rx,
            reported_dead: std::collections::HashSet::new(),
        })
    }

    /// Check for crashed/exited node threads. Returns names of newly dead nodes
    /// (each node is reported only once).
    pub fn health_check(&mut self) -> Vec<String> {
        let mut newly_dead = Vec::new();
        for handle in &self._threads {
            if handle.is_finished() {
                let name = handle
                    .thread()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string();
                if self.reported_dead.insert(name.clone()) {
                    newly_dead.push(name);
                }
            }
        }
        newly_dead
    }

    /// Drain all emit channels, route to OUT channels, and return all emitted turtles.
    /// Call this from tick() to pump script outputs into the DAG.
    pub fn pump_emits(&self) -> Vec<String> {
        let mut collected = Vec::new();
        for route in &self.emit_routes {
            while let Ok(turtle) = route.rx.try_recv() {
                for out_uri in &route.out_uris {
                    self.broadcast(out_uri, &turtle);
                }
                collected.push(turtle);
            }
        }
        collected
    }

    /// Process store query requests from script threads.
    pub fn pump_queries(&self, store: &RdfStore) {
        while let Ok((sparql, resp_tx)) = self.query_rx.try_recv() {
            let result = match store.query(&sparql) {
                Ok(results) => {
                    if let QueryResults::Solutions(solutions) = results {
                        solutions
                            .filter_map(|s| s.ok())
                            .map(|sol| {
                                sol.iter()
                                    .map(|(var, term)| {
                                        // Strip typed literal wrappers for cleaner JS access
                                        let val = match term {
                                            Term::Literal(lit) => lit.value().to_string(),
                                            Term::NamedNode(nn) => nn.as_str().to_string(),
                                            _ => term.to_string(),
                                        };
                                        (var.as_str().to_string(), val)
                                    })
                                    .collect()
                            })
                            .collect()
                    } else {
                        vec![]
                    }
                }
                Err(e) => {
                    tracing::warn!(%e, "script store.query error");
                    vec![]
                }
            };
            let _ = resp_tx.send(result);
        }
    }

    /// Broadcast a Turtle string to all subscribers of a named channel.
    pub fn broadcast(&self, channel_uri: &str, turtle: &str) {
        if let Some(writers) = self.channel_writers.get(channel_uri) {
            for writer in writers {
                let _ = writer.send(turtle);
            }
        }
    }

    /// Broadcast to the beforeInsert channel.
    pub fn before_insert(&self, turtle: &str) {
        self.broadcast(&format!("{}beforeInsert", ANTENNA_NS), turtle);
    }

    /// Broadcast to the afterInsert channel.
    pub fn after_insert(&self, turtle: &str) {
        self.broadcast(&format!("{}afterInsert", ANTENNA_NS), turtle);
    }
}

// ---------------------------------------------------------------------------
// Query Oxigraph for ScriptNode definitions
// ---------------------------------------------------------------------------

fn query_script_nodes(store: &RdfStore) -> Result<Vec<ScriptNodeDef>> {
    let sparql = r#"
        PREFIX antenna: <http://resonator.network/v2/antenna#>
        SELECT ?node ?body ?language ?in ?out ?where WHERE {
            ?node a antenna:ScriptNode ;
                  antenna:scriptSource ?src .
            ?src antenna:body ?body .
            OPTIONAL { ?src antenna:language ?language }
            OPTIONAL { ?node antenna:in ?in }
            OPTIONAL { ?node antenna:out ?out }
            OPTIONAL { ?node antenna:where ?where }
        }
    "#;

    let results = store.query(sparql)?;
    let mut node_map: HashMap<String, ScriptNodeDef> = HashMap::new();

    if let QueryResults::Solutions(solutions) = results {
        for solution in solutions {
            let solution = solution?;
            let uri = term_to_string(solution.get("node"));
            let body = term_to_string(solution.get("body"));
            let language =
                optional_term(solution.get("language")).unwrap_or_else(|| "javascript".to_string());
            let in_ch = optional_term(solution.get("in"));
            let out_ch = optional_term(solution.get("out"));
            let where_clause = optional_term(solution.get("where"));

            let node = node_map
                .entry(uri.clone())
                .or_insert_with(|| ScriptNodeDef {
                    uri,
                    body,
                    language,
                    ins: Vec::new(),
                    outs: Vec::new(),
                    where_clause,
                });

            if let Some(ch) = in_ch {
                if !node.ins.contains(&ch) {
                    node.ins.push(ch);
                }
            }
            if let Some(ch) = out_ch {
                if !node.outs.contains(&ch) {
                    node.outs.push(ch);
                }
            }
        }
    }

    Ok(node_map.into_values().collect())
}

fn term_to_string(term: Option<&Term>) -> String {
    match term {
        Some(Term::Literal(lit)) => lit.value().to_string(),
        Some(Term::NamedNode(node)) => node.as_str().to_string(),
        Some(Term::BlankNode(bn)) => bn.as_str().to_string(),
        _ => String::new(),
    }
}

fn optional_term(term: Option<&Term>) -> Option<String> {
    match term {
        Some(Term::Literal(lit)) => Some(lit.value().to_string()),
        Some(Term::NamedNode(node)) => Some(node.as_str().to_string()),
        Some(Term::BlankNode(bn)) => Some(bn.as_str().to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Query Oxigraph for SemanticRouter definitions
// ---------------------------------------------------------------------------

fn query_semantic_routers(store: &RdfStore) -> Result<Vec<SemanticRouterDef>> {
    let sparql = r#"
        PREFIX antenna: <http://resonator.network/v2/antenna#>
        SELECT ?node ?inA ?inB ?out ?backendType ?endpoint ?model ?systemPrompt ?maxTokens WHERE {
            ?node a antenna:SemanticRouter ;
                  antenna:inA ?inA ;
                  antenna:inB ?inB ;
                  antenna:llmBackend ?backend .
            ?backend antenna:backendType ?backendType .
            OPTIONAL { ?backend antenna:endpoint ?endpoint }
            OPTIONAL { ?backend antenna:model ?model }
            OPTIONAL { ?node antenna:out ?out }
            OPTIONAL { ?node antenna:systemPrompt ?systemPrompt }
            OPTIONAL { ?node antenna:maxTokens ?maxTokens }
        }
    "#;

    let results = store.query(sparql)?;
    let mut router_map: HashMap<String, SemanticRouterDef> = HashMap::new();

    if let QueryResults::Solutions(solutions) = results {
        for solution in solutions {
            let solution = solution?;
            let uri = term_to_string(solution.get("node"));
            let in_a = term_to_string(solution.get("inA"));
            let in_b = term_to_string(solution.get("inB"));
            let out_ch = optional_term(solution.get("out"));
            let backend_type = term_to_string(solution.get("backendType"));
            let endpoint = optional_term(solution.get("endpoint")).unwrap_or_default();
            let model =
                optional_term(solution.get("model")).unwrap_or_else(|| "default".to_string());
            let system_prompt = optional_term(solution.get("systemPrompt")).unwrap_or_default();
            let max_tokens = optional_term(solution.get("maxTokens"))
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(512);

            let router = router_map
                .entry(uri.clone())
                .or_insert_with(|| SemanticRouterDef {
                    uri,
                    in_a,
                    in_b,
                    outs: Vec::new(),
                    backend_type,
                    endpoint,
                    model,
                    system_prompt,
                    max_tokens,
                });

            if let Some(ch) = out_ch {
                if !router.outs.contains(&ch) {
                    router.outs.push(ch);
                }
            }
        }
    }

    Ok(router_map.into_values().collect())
}

fn short_uri(uri: &str) -> &str {
    uri.rsplit_once('#')
        .or_else(|| uri.rsplit_once('/'))
        .or_else(|| uri.rsplit_once(':'))
        .map(|(_, name)| name)
        .unwrap_or(uri)
}
