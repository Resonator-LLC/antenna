# Antenna

RDF stream processor with Tox P2P networking, QuickJS scripting, and
SPARQL-capable RDF store.

Antenna processes everything as RDF Turtle — events, queries, commands, and data
flow through the same pipeline. It dispatches by `rdf:type`: SPIN queries go to
the SPARQL engine, Tox commands go to the P2P network, everything else routes
through a scriptable DAG and into the embedded store.

## Architecture

```
                         ┌──────────────────────────────────────────┐
                         │               Antenna                    │
                         │                                          │
  Turtle IN ──────────►  │  Dispatch ─► SPIN ──► Oxigraph Store     │
  (pipe/ws/channel)      │     │                       ▲            │
                         │     ├──────► Tox ───► P2P Network        │
                         │     │                                    │
                         │     └──────► DAG ───► Script Nodes       │
                         │              │         (QuickJS)         │
                         │              │            │              │
                         │              ▼            ▼              │
  Turtle OUT ◄────────── │         Channel Router ──► Store         │
  (pipe/ws/channel)      │                                          │
                         └──────────────────────────────────────────┘
```

**Key design choices:**

- **Turtle in, Turtle out** — no custom protocol; SPIN RDF for queries, carrier
  namespace for P2P commands
- **Reactive dispatch** — Antenna recognizes types it supports and acts; unknown
  types get stored
- **Thread-per-node DAG** — each script node runs in its own thread with
  lock-free SPSC channels and clock-fd signaling (eventfd on Linux, self-pipe
  on macOS)
- **Transport-agnostic** — `AntennaIn`/`AntennaOut` traits abstract pipes,
  WebSocket, and ring buffer channels (for Dart/Swift/C FFI embedding)
- **LLM integration** — SemanticRouter nodes synthesize RDF from two input
  graphs using Ollama, OpenAI-compatible, or on-device backends

## Prerequisites

- Rust stable (edition 2021)
- C11 compiler (gcc or clang)
- pkg-config
- System libraries: **serd**, **toxcore**, **QuickJS**, **libsodium**, **opus**, **libvpx**

### macOS

```bash
brew install serd toxcore quickjs libsodium opus libvpx pkg-config
```

### Debian/Ubuntu (trixie, sid, or Ubuntu 25.04+)

```bash
sudo apt install build-essential pkg-config libserd-dev libtoxcore-dev \
  libquickjs-dev libsodium-dev libopus-dev libvpx-dev
```

### Debian bookworm / Ubuntu 24.04

QuickJS is not packaged in Debian bookworm or Ubuntu 24.04. Install the other
dependencies from apt, then build QuickJS from source:

```bash
sudo apt install build-essential pkg-config libserd-dev libtoxcore-dev \
  libsodium-dev libopus-dev libvpx-dev

git clone https://github.com/nicbarker/quickjs-standalone.git /tmp/quickjs
cd /tmp/quickjs && make -j$(nproc)
sudo make install PREFIX=/usr/local
```

Then set the env var so the build script can find it:

```bash
export QUICKJS_DIR=/usr/local
```

### Fedora

QuickJS is not packaged in Fedora. Install the rest from dnf, then build
QuickJS from source (same steps as above):

```bash
sudo dnf install gcc gcc-c++ make pkg-config serd-devel toxcore-devel \
  libsodium-devel opus-devel libvpx-devel
```

## Build

```bash
git clone --recursive https://source.resonator.network/resonator/antenna.git
cd antenna
cargo build --release
```

If you already cloned without `--recursive`, fetch the carrier submodule:

```bash
git submodule update --init
```

### Environment variable overrides

| Variable | Purpose |
|----------|---------|
| `CARRIER_DIR` | Path to carrier source (default: `third_party/carrier` submodule) |
| `QUICKJS_DIR` | QuickJS install prefix (default: auto-detected via Homebrew or `/usr/local`) |

## Run

### Pipe mode (stdin/stdout)

```bash
echo '[] a carrier:GetId .' | ./target/release/antenna --profile /tmp/resonator.tox
```

### WebSocket mode

```bash
./target/release/antenna \
  --profile /tmp/resonator.tox \
  --seed seed.ttl \
  --ws 9903
```

### Composable

```bash
rdf-source | antenna -p my.tox | rdf-sink
```

## CLI Options

| Flag | Type | Required | Description |
|------|------|----------|-------------|
| `-p, --profile` | PATH | Yes | Tox identity file (created on first run) |
| `--ws` | PORT | No | Start WebSocket server on PORT |
| `--store` | PATH | No | Oxigraph RDF store directory (omit for in-memory) |
| `--pipeline` | PATH | No | Turtle file with pipeline DAG definition |
| `--nodes` | PATH | No | JSON file with DHT bootstrap nodes |
| `--seed` | PATH | No | Turtle file to load as seed data at startup |

## Protocol Examples

### SPARQL queries (via SPIN)

```turtle
# SELECT
[] a sp:Select ; sp:text "SELECT ?s ?text WHERE { ?s a carrier:TextMessage ; carrier:text ?text }" .

# ASK
[] a sp:Ask ; sp:text "ASK { ?s a carrier:Connected }" .

# CONSTRUCT
[] a sp:Construct ; sp:text "CONSTRUCT { ?s a antenna:Seen } WHERE { ?s a carrier:TextMessage }" .

# UPDATE
[] a sp:InsertData ; sp:text "INSERT DATA { <urn:x> a <urn:Foo> }" .
```

### Tox P2P commands

```turtle
[] a carrier:GetId .
[] a carrier:SetNick ; carrier:nick "mynode" .
[] a carrier:SendMsg ; carrier:friendId 0 ; carrier:text "hello" .
```

### Raw RDF (stored and routed through DAG)

```turtle
<urn:bookmark:1> a antenna:Bookmark ; rdfs:label "interesting" .
```

## Script DAG

Processing nodes are defined as RDF and loaded from the store:

```turtle
<urn:src:logger> a antenna:ScriptSource ;
    antenna:body "emit(input);" .

<urn:node:logger> a antenna:ScriptNode ;
    antenna:scriptSource <urn:src:logger> ;
    antenna:in antenna:beforeInsert ;
    antenna:out antenna:mainOut .
```

Scripts run in QuickJS with these globals:

- `input` — Turtle string from the triggering channel
- `channel` — URI of the channel that delivered the input
- `emit(turtle)` — send Turtle to all OUT channels
- `store.query(sparql)` — synchronous SPARQL SELECT, returns array of objects

## Test

```bash
cargo test
```

## License

[MIT License](LICENSE)
