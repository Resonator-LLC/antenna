use anyhow::Result;
use clap::Parser;

use antenna::carrier_tox::TURTLE_PREFIXES;
use antenna::channel::{AntennaOut, PipeIn, PipeOut};
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("antenna=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();

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
