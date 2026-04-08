# Antenna — Stage 2

Composable RDF stream processor. Turtle in, Turtle out. Reactive — recognizes types it supports, stores everything else.

## IN/OUT Interface

Antenna exposes two streams:

- **IN** — receives RDF Turtle (commands, queries, raw data)
- **OUT** — emits RDF Turtle (events, results, script output)

Cross-connected to any client (Dart, CLI, another process):

```
App OUT → Antenna IN
App IN  ← Antenna OUT
```

Transport-agnostic via traits:

```rust
pub trait AntennaIn {
    fn recv(&mut self) -> Option<String>;
    fn clock_fd(&self) -> Option<RawFd>;
}

pub trait AntennaOut {
    fn send(&mut self, turtle: &str);
}
```

Implementations:

- **RingChannel** — SPSC ring buffer + clock fd (FFI: Dart, C, Swift)
- **Pipe** — stdin/stdout (CLI mode)
- **Channel** — mpsc (Rust-to-Rust)

## Clock Signals

Each channel direction has its own clock fd. No polling — event-driven wake.

```
Platform        fd type       Create               Signal         Consume
Linux/Android   eventfd       eventfd(0, EFD_NB)   write(fd, 1)   read(fd, &buf)
macOS/iOS       self-pipe     pipe2(O_NONBLOCK)     write(fd, 1)   read(fd, &buf)
```

Writer: `write(ring, msg)` → `write(clock_fd, 1)`
Reader: `poll(clock_fd)` → `read(clock_fd)` → `read(ring)`

2 clock fds per external channel pair. Internal DAG channels also have clocks.

### Ring Buffer Layout

```
[u32 head (atomic)] [u32 tail (atomic)] [u8; capacity]
Messages: [u32 len][utf8 data][padding to 4-byte align]
```

SPSC lock-free: writer advances head, reader advances tail. `Ordering::Release`/`Acquire`.

## Protocol — SPIN RDF

No custom command vocabulary. Uses [SPIN](https://spinrdf.org/) — SPARQL queries represented as RDF.

```turtle
@prefix sp:   <http://spinrdf.org/sp#> .
@prefix spin: <http://spinrdf.org/spin#> .
```

Antenna is purely reactive. It receives RDF on IN and dispatches by `rdf:type`:

| Type received | Reaction |
|---|---|
| `sp:Select` | Evaluate `sp:text` SPARQL, emit result triples on OUT |
| `sp:Ask` | Evaluate SPARQL ASK, emit boolean result on OUT |
| `sp:Construct` | Evaluate, insert constructed triples, emit on OUT |
| `sp:InsertData` | Execute INSERT DATA against store |
| `sp:DeleteData` | Execute DELETE DATA against store |
| `sp:Modify` | Execute DELETE/INSERT WHERE against store |
| `tox:*` | Forward to libcarrier-tox (Tox P2P network) |
| Anything else | Route through script DAG → insert into store → emit on OUT |

### Examples on IN

```turtle
[] a sp:Select ; sp:text "SELECT ?s ?text WHERE { ?s a tox:TextMessage ; tox:text ?text }" .

[] a sp:Ask ; sp:text "ASK { ?s a tox:Connected }" .

[] a sp:Construct ; sp:text "CONSTRUCT { ?s a antenna:Seen } WHERE { ?s a tox:TextMessage }" .

[] a tox:GetId .
[] a tox:SetNick ; tox:nick "mynode" .

<urn:bookmark:1> a antenna:Bookmark ; rdfs:label "interesting" .
```

### Examples on OUT

```turtle
[] a tox:Connected ; tox:transport "UDP" ; tox:at "2026-03-25T10:00:00"^^xsd:dateTime .
[] a tox:TextMessage ; tox:friendId 0 ; tox:text "hi" ; tox:at "..." .

[] a sp:AskResult ; sp:boolean true .

[] a antenna:Error ; antenna:message "SPARQL parse error at line 1" .
```

### Design Principle

No commands, no responses. Only RDF objects flowing in and out. If antenna recognizes the type, it acts. If not, it stores it.

## Script DAG — Channel-Based Processing Graph

Scripts are processing nodes with named IN and OUT channels. The DAG topology emerges from channel connections.

### Channels

A channel is a named, clocked message bus. Built-in:

| Channel | Description |
|---|---|
| `antenna:mainIn` | External IN (from app/stdin) |
| `antenna:mainOut` | External OUT (to app/stdout) |
| `antenna:beforeInsert` | Fires before each store insert |
| `antenna:afterInsert` | Fires after each store insert |

User-defined channels:

```turtle
<urn:channel:enriched> a antenna:Channel .
<urn:channel:alerts> a antenna:Channel .
```

### ScriptNode

A processing node declares its channel connections:

```turtle
<urn:node:enricher> a antenna:ScriptNode ;
    antenna:scriptSource <urn:source:enricher> ;
    antenna:in antenna:beforeInsert ;
    antenna:out <urn:channel:enriched> .

<urn:node:logger> a antenna:ScriptNode ;
    antenna:scriptSource <urn:source:logger> ;
    antenna:in <urn:channel:enriched> ;
    antenna:out antenna:mainOut .

<urn:node:alerter> a antenna:ScriptNode ;
    antenna:scriptSource <urn:source:alerter> ;
    antenna:in antenna:afterInsert ;
    antenna:in <urn:channel:enriched> ;
    antenna:out <urn:channel:alerts> ;
    antenna:where "?s a tox:TextMessage" .
```

DAG emerges:

```
mainIn → [dispatch] → beforeInsert → [enricher] → enriched → [logger] → mainOut
                                                      ↓
                    → store insert → afterInsert → [alerter] → alerts
                                                      ↑
                                                   enriched
```

### ScriptSource

Language-tagged code artifact. JavaScript (QuickJS) by default, extensible:

```turtle
<urn:source:enricher> a antenna:ScriptSource ;
    antenna:creator <urn:user:rafael> ;
    antenna:signature "a1b2c3..." ;
    antenna:createdAt "2026-03-25T12:00:00"^^xsd:dateTime ;
    antenna:description "Add processing timestamp" ;
    antenna:language "javascript" ;
    antenna:body "emit(input);" .
```

### Script Runtime Contract

```javascript
// Globals:
//   input     — Turtle string from the IN channel that woke this script
//   channel   — URI of the triggering IN channel
//   emit(s)   — write Turtle to all OUT channels (signals their clocks)
//   store     — { query(sparql), ask(sparql), insert(turtle) }
```

Multiple INs: script wakes when any IN has data. `channel` tells which one fired.

### Internal Channel Clocks

Each internal channel has a ring buffer + clock fd. Script node event loop:

1. `poll()` on all IN channel clock fds
2. Wake → read from ready channel
3. Execute script with `input` + `channel`
4. `emit(turtle)` → writes to each OUT channel → signals their clocks
5. Loop

### Live RDF

The entire DAG lives in Oxigraph. Queryable, updatable, hot-reloadable:

```turtle
# Query the DAG
[] a sp:Select ; sp:text "SELECT ?node ?in ?out WHERE { ?node a antenna:ScriptNode ; antenna:in ?in ; antenna:out ?out }" .

# Add a node at runtime (just send the triples)
<urn:src:new> a antenna:ScriptSource ;
    antenna:body "print(input);" .
<urn:node:new> a antenna:ScriptNode ;
    antenna:scriptSource <urn:src:new> ;
    antenna:in antenna:afterInsert ;
    antenna:out antenna:mainOut .

# Remove a node
[] a sp:Modify ; sp:text "DELETE WHERE { <urn:node:new> ?p ?o }" .
```

Antenna watches for changes to `antenna:ScriptNode`/`antenna:ScriptSource`/`antenna:Channel` and hot-reloads.

## Project Structure

```
antenna/
  Cargo.toml
  build.rs            cc: compile carrier + serd + quickjs from source
  src/
    lib.rs            AntennaContext, tick(), run()
    main.rs           CLI: stdin/stdout as IN/OUT
    carrier_tox.rs    FFI bindings to carrier.h, event→Turtle serializer
    store.rs          Oxigraph embedded store wrapper
    dag.rs            Channel graph, ScriptNode loading, internal clocks, routing
    script_vm.rs      QuickJS C FFI, JS runtime, input/channel/emit/store globals
    dispatch.rs       Reactive router: parse Turtle type → SPIN/tox/store
    channel.rs        AntennaIn/Out traits, RingChannel, PipeTransport, clock fds
    ffi.rs            C API: antenna_channel_*, antenna_new/free/tick/run
  ontology/
    pipeline.ttl      Antenna + SPIN vocabulary
```

## C API (for Dart/FFI)

```c
// Channel pair (2 ring buffers + 2 clock fds)
antenna_channel* antenna_channel_new(size_t capacity);
void             antenna_channel_free(antenna_channel* ch);
int              antenna_channel_send(antenna_channel* ch, const char* data, size_t len);
int              antenna_channel_recv(antenna_channel* ch, char* buf, size_t buf_len);
int              antenna_channel_clock_fd(antenna_channel* ch);
void             antenna_channel_clock_consume(antenna_channel* ch);

// Antenna instance
antenna_ctx*     antenna_new(const char* profile, const char* store_path,
                             const char* pipeline_path, const char* nodes_path,
                             antenna_channel* ch);
void             antenna_free(antenna_ctx* ctx);
int              antenna_tick(antenna_ctx* ctx);
int              antenna_run(antenna_ctx* ctx);
int              antenna_iteration_interval(antenna_ctx* ctx);
```

## Dart Usage

```dart
final ch = antennaChannelNew(65536);
final ctx = antennaNew(profile, storePath, pipelinePath, nodesPath, ch);

// Isolate 1: antenna event loop (blocks)
Isolate.spawn((_) => antennaRun(ctx));

// Isolate 2: watch OUT clock, forward to main
final clockFd = antennaChannelClockFd(ch);
final recvPort = ReceivePort();
Isolate.spawn((sendPort) {
  final buf = calloc<Uint8>(8192);
  while (true) {
    poll(clockFd, POLLIN, -1);
    antennaChannelClockConsume(ch);
    while (true) {
      final n = antennaChannelRecv(ch, buf, 8192);
      if (n == 0) break;
      sendPort.send(buf.cast<Utf8>().toDartString(length: n));
    }
  }
}, recvPort.sendPort);

// Main: events as a stream
recvPort.listen((turtle) => print('Received: $turtle'));

// Send SPIN query
antennaChannelSend(ch, '[] a sp:Select ; sp:text "SELECT ?s WHERE { ?s a tox:TextMessage }" .');
```

## CLI Usage

```sh
# Interactive
antenna -p my.tox --store ./data --pipeline dag.ttl

# Pipe
echo '[] a sp:Ask ; sp:text "ASK { ?s a tox:Connected }" .' | antenna -p my.tox

# Composable
rdf-source | antenna -p my.tox | rdf-sink
```

## Dependencies

```toml
[dependencies]
oxigraph = "0.4"
clap = { version = "4", features = ["derive"] }
anyhow = "1"

[build-dependencies]
cc = "1"

[lib]
name = "antenna"
crate-type = ["cdylib", "rlib"]
```

- QuickJS compiled from source via `cc` in build.rs (no Rust crate)
- Carrier (tox) compiled from source via `cc` in build.rs
- Serd compiled from source via `cc` in build.rs
- `mlua` removed — replaced by direct QuickJS C FFI
