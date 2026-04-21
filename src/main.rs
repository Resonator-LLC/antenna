// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

use anyhow::Result;
use clap::Parser;

use antenna::carrier_tox::TURTLE_PREFIXES;
use antenna::channel::{AntennaOut, PipeIn, PipeOut};
use antenna::logging;
use antenna::ws;
use antenna::AntennaContext;

#[derive(Parser)]
#[command(
    name = "antenna",
    about = "RDF stream processor with Tox P2P and QuickJS scripting"
)]
struct Args {
    /// Carrier .tox profile path
    #[arg(long, short)]
    profile: String,

    /// Oxigraph store directory (omit for in-memory)
    #[arg(long)]
    store: Option<String>,

    /// Turtle file with pipeline DAG definition to load at startup
    #[arg(long)]
    pipeline: Option<String>,

    /// Path to DHT bootstrap nodes JSON file
    #[arg(long)]
    nodes: Option<String>,

    /// Turtle file to load as seed data at startup
    #[arg(long)]
    seed: Option<String>,

    /// Start WebSocket server on this port (default: use stdin/stdout pipes)
    #[arg(long)]
    ws: Option<u16>,

    /// Shorthand for --log debug (bumps everything antenna-owned to DEBUG).
    /// Dev-session default. RUST_LOG always wins if set.
    #[arg(long, default_value_t = false)]
    debug: bool,

    /// Log level for antenna (error/warn/info/debug/trace). Default: warn.
    /// Overridden by RUST_LOG if that env var is set.
    #[arg(long, value_name = "LEVEL")]
    log: Option<String>,

    /// Restrict log output to a comma-separated list of tags, e.g.
    /// --log-tags TOX,DHT,WS. Unknown tags are silently ignored.
    /// Tag matching is performed by the formatter — records with other
    /// tags are dropped before printing. Empty string = no restriction.
    #[arg(long, value_name = "TAGS", default_value = "")]
    log_tags: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Resolve the antenna-side default level. Precedence:
    //   1. RUST_LOG env var (always wins if present)
    //   2. --log LEVEL
    //   3. --debug shorthand
    //   4. Fallback: warn
    let level = if let Some(l) = args.log.as_deref() {
        l.to_string()
    } else if args.debug {
        "debug".to_string()
    } else {
        "warn".to_string()
    };

    logging::init(&level, &args.log_tags)?;

    let mut ctx = AntennaContext::new(
        &args.profile,
        args.store.as_deref(),
        args.pipeline.as_deref(),
        args.nodes.as_deref(),
        args.seed.as_deref(),
    )?;

    if let Some(port) = args.ws {
        // WebSocket mode — greeting sent to each new client
        let (mut ws_in, mut ws_out) =
            ws::start_ws_server(port, Some(TURTLE_PREFIXES.trim().to_string()))?;

        // Run event loop with WS transport
        ctx.run(&mut ws_in, &mut ws_out)?;
    } else {
        // Pipe mode (stdin/stdout)
        let mut input = PipeIn::new();
        let mut output = PipeOut::new();

        output.send(TURTLE_PREFIXES.trim());

        ctx.run(&mut input, &mut output)?;
    }

    Ok(())
}
