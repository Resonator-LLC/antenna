# Architecture

This document describes the internal architecture of Antenna. For usage
instructions, see [README.md](README.md).

## Module Overview

```
src/
  lib.rs            AntennaContext, main tick loop, event orchestration
  main.rs           CLI entry point (clap), WebSocket or pipe mode selection
  channel.rs        AntennaIn/Out traits, Clock, RingBuffer, InternalChannel, PipeIn/PipeOut
  store.rs          Oxigraph RDF store wrapper
  dispatch.rs       Reactive router: parse Turtle type -> SPIN/Tox/store dispatch
  dag.rs            Channel-based script DAG, thread spawning, SemanticRouter
  script_vm.rs      QuickJS C FFI: runtime, context, JS globals (emit, print, store)
  carrier_tox.rs    Tox P2P FFI bindings, CarrierEvent -> Turtle serialization
  llm.rs            LLM backend abstraction (Ollama, HTTP, Platform)
  ws.rs             WebSocket server: multi-client, sequential accept
  quickjs_shim.c    C shim for QuickJS static inline functions
```

## Data Flow

```
Tox Event (C callback)
  -> event_to_turtle() serialization
  -> mpsc channel -> main tick loop
  -> output + DAG beforeInsert + store insert + DAG afterInsert

External Input (pipe/ws/channel)
  -> dispatch by rdf:type:
       sp:Select/Ask/Construct  -> SPARQL on Oxigraph -> output
       sp:InsertData/DeleteData -> SPARQL UPDATE
       carrier:GetId/SetNick/.. -> carrier FFI call
       anything else            -> DAG beforeInsert -> store insert -> DAG afterInsert -> output

Script emit(turtle)
  -> channel broadcast to OUT channels
  -> pump_emits() drains to store + output
```

## Thread Model

```
Main thread:
  - Owns AntennaContext
  - Runs tick() in a loop (poll-based)
  - Processes: carrier iteration, event draining, input dispatch,
    query handling, emit pumping

Per ScriptNode thread:
  - Owns a ScriptVm (QuickJS runtime)
  - Blocks on poll() over its IN channel clock fds
  - Executes JS source on each message
  - emit() sends to mpsc, drained by main thread

Per SemanticRouter thread:
  - Blocks on poll() over two IN channel clock fds (inA, inB)
  - Latches latest from each, fires LLM when both filled
  - Validates output as Turtle, one retry on parse error

WebSocket accept thread:
  - Accepts TCP connections sequentially
  - Per-client forwarding thread for output

Carrier event thread:
  - Not a separate thread — events arrive via C callback during
    carrier_iterate() on the main thread
```

## Channel System

Internal channels use a lock-free SPSC ring buffer with clock fd signaling:

```
Writer: push(data) -> ring buffer -> signal(clock_fd)
Reader: poll(clock_fd) -> consume_clock() -> pop() from ring buffer

Ring buffer layout:
  [u32 head (atomic)] [u32 tail (atomic)] [u8; capacity]
  Messages: [u32 len (LE)] [utf8 data]

Clock:
  Linux:  eventfd(0, EFD_NONBLOCK)  -- single fd for both read/write
  macOS:  pipe() with O_NONBLOCK    -- read_fd + write_fd
```

DAG channels are named URIs. Built-in channels:

- `antenna:mainIn` — external input
- `antenna:mainOut` — external output
- `antenna:beforeInsert` — fires before each store insert
- `antenna:afterInsert` — fires after each store insert
- `antenna:clock` — periodic tick signal

User-defined channels connect script nodes into arbitrary topologies.

## FFI Boundaries

### libcarrier (Tox P2P)

- `carrier_tox.rs` defines `#[repr(C)]` types matching `carrier.h`
- 29 event types in a tagged union (`CarrierEventType` + `CarrierEventData`)
- Event callback registered via opaque pointer to `Sender<String>`
- Safe wrapper: `ToxCarrier` with `iterate()`, `send_message()`, etc.

### QuickJS (JavaScript engine)

- `script_vm.rs` declares extern C functions matching QuickJS API
- `quickjs_shim.c` wraps static inline functions that can't be called from Rust
- `ScriptVm` owns a `JSRuntime` + `JSContext` with registered globals
- JS globals (`emit`, `print`, `store.query`) use `JS_GetContextOpaque`
  to access a `VmOpaque` struct holding Rust senders

### Build System

`build.rs` compiles carrier from the git submodule (`third_party/carrier`)
and links system-installed libraries for everything else:

- **serd** — found via `pkg-config serd-0`
- **toxcore** — found via `pkg-config toxcore` (pulls in libsodium, opus, vpx)
- **quickjs** — located via Homebrew prefix or standard system paths
- **carrier** — compiled from source (`third_party/carrier` submodule, 2 C files)

`CARRIER_DIR` and `QUICKJS_DIR` environment variables can override the defaults.

## Store

Thin wrapper around Oxigraph (`oxigraph::store::Store`):

- In-memory or persistent (directory-backed)
- All Turtle input auto-prepended with standard prefixes
- Supports named graphs for isolation (used by DAG guards)
- SPARQL SELECT, ASK, CONSTRUCT, UPDATE

## Dispatch

Lightweight Turtle type extraction without a full parser:

1. Find ` a ` pattern in the line
2. Resolve known prefixes (sp:, carrier:, antenna:, etc.)
3. Route to handler by namespace

Property extraction follows the same lightweight approach — suitable for
single-line Turtle statements which is the primary message format.

## LLM Integration

Three backend types behind the `LlmBackend` trait:

- **OllamaBackend** — HTTP POST to `/api/generate`
- **HttpBackend** — OpenAI-compatible `/chat/completions`
- **PlatformBackend** — Channel-based IPC to host app (iOS/Android on-device models)

SemanticRouter nodes latch two inputs, build a prompt with both graphs,
call the backend, validate the output as Turtle, and emit the result.
