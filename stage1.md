# Antenna — Stage 1

Rust project that links libcarrier directly (C FFI), stores all received RDF into an embedded Oxigraph database, and runs Lua scripts (via mlua) in a DAG before/after each insert. The DAG itself is defined as RDF in Oxigraph. Targets: macOS, Linux, iOS, Android.

## Project Structure

```
antenna/
  Cargo.toml
  build.rs              cc crate: compile libcarrier + serd from source
  src/
    main.rs             CLI args (clap), main loop, signal handling
    carrier.rs          FFI bindings to carrier.h, event callback, Turtle serializer
    store.rs            Oxigraph embedded store wrapper
    pipeline.rs         DAG loader (SPARQL), topo-sort, script chain execution
    lua_vm.rs           mlua VM setup, sandboxing, script invocation
  ontology/
    pipeline.ttl        Antenna pipeline vocabulary
```

## Dependencies

```toml
[dependencies]
oxigraph = "0.4"
mlua = { version = "0.10", features = ["lua54", "vendored"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"

[build-dependencies]
cc = "1"
```

- `mlua` vendors Lua 5.4 from source (no system Lua needed)
- `cc` compiles libcarrier + serd from source in `build.rs`
- Links against toxcore (static, from `../deps/lib`) and its transitive deps (libsodium, opus, vpx) via pkg-config

## Build System (`build.rs`)

Compiles two C11 libraries from the monorepo source:

1. **serd** — Turtle RDF parser (11 source files from `../serd/src/`), no external deps
2. **carrier** — Tox wrapper (`carrier.c`, `carrier_events.c` from `../carrier/src/`), depends on toxcore headers + serd

Links toxcore statically from `../deps/lib/libtoxcore.a`. Finds libsodium/opus/vpx via pkg-config for portability.

## Ontology

Namespace: `http://resonator.network/v2/antenna#`

### ScriptSource — first-class, signed, transmittable artifact

A `antenna:ScriptSource` carries Lua code as a standalone entity. It can be created on one node, sent over carrier as RDF, and stored/executed on another.

| Property | Range | Description |
|---|---|---|
| `antenna:body` | `xsd:string` | Lua source code |
| `antenna:creator` | URI | Author identifier |
| `antenna:signature` | `xsd:string` | Cryptographic signature (e.g. Ed25519 hex) |
| `antenna:createdAt` | `xsd:dateTime` | Creation timestamp |
| `antenna:description` | `xsd:string` | Human-readable description |

### Pipeline DAG — references ScriptSources

| Class/Property | Range | Description |
|---|---|---|
| `antenna:Pipeline` | class | Named pipeline with an entrypoint |
| `antenna:Script` | class | Node in the DAG, references a ScriptSource |
| `antenna:entrypoint` | `antenna:Script` | First script in a pipeline |
| `antenna:scriptSource` | `antenna:ScriptSource` | Links node to its code |
| `antenna:hook` | `xsd:string` | `"before-insert"` or `"after-insert"` |
| `antenna:next` | `antenna:Script` | Edge to the next script in chain |
| `antenna:priority` | `xsd:integer` | Execution order (lower first, default 0) |
| `antenna:where` | `xsd:string` | SPARQL WHERE clause guard — script runs only if `ASK { <clause> }` is true |
| `antenna:unless` | `xsd:string` | Inverse guard — script runs only if `ASK { <clause> }` is false |

### Guard Evaluation

Before executing a script node, antenna constructs `ASK { <where-clause> }` and runs it against the store. For `before-insert` hooks the incoming triples are inserted into a temporary named graph `<urn:antenna:pending>` so the WHERE clause can match against data not yet in the store. After evaluation the temp graph is dropped.

Guard failure skips the script but continues DAG traversal to `next`.

### Example Pipeline

```turtle
@prefix antenna: <http://resonator.network/v2/antenna#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

# Script sources — portable, signed artifacts
<urn:source:logger> a antenna:ScriptSource ;
    antenna:creator <urn:user:rafael> ;
    antenna:signature "a1b2c3..." ;
    antenna:createdAt "2026-03-24T12:00:00"^^xsd:dateTime ;
    antenna:description "Log all incoming RDF to stdout" ;
    antenna:body "print('[log] ' .. input)\nreturn input" .

<urn:source:enrich> a antenna:ScriptSource ;
    antenna:creator <urn:user:rafael> ;
    antenna:signature "d4e5f6..." ;
    antenna:createdAt "2026-03-24T12:00:00"^^xsd:dateTime ;
    antenna:description "Enrich messages with metadata" ;
    antenna:body "return input .. ' ; antenna:processed true'" .

<urn:source:notify> a antenna:ScriptSource ;
    antenna:creator <urn:user:rafael> ;
    antenna:signature "789abc..." ;
    antenna:createdAt "2026-03-24T12:05:00"^^xsd:dateTime ;
    antenna:description "Emit notification triple after insert" ;
    antenna:body "return '[] a antenna:Notification ; antenna:text \"arrived\" .'" .

# Pipeline DAG — references the sources
<urn:pipeline:main> a antenna:Pipeline ;
    antenna:entrypoint <urn:script:logger> .

<urn:script:logger> a antenna:Script ;
    antenna:scriptSource <urn:source:logger> ;
    antenna:hook "before-insert" ;
    antenna:priority 0 ;
    antenna:next <urn:script:enrich> .

# Only enrich if the incoming data is a TextMessage
<urn:script:enrich> a antenna:Script ;
    antenna:scriptSource <urn:source:enrich> ;
    antenna:hook "before-insert" ;
    antenna:priority 1 ;
    antenna:where "?s a carrier:TextMessage" .

# Notify after insert, but skip if it's an error event
<urn:script:notify> a antenna:Script ;
    antenna:scriptSource <urn:source:notify> ;
    antenna:hook "after-insert" ;
    antenna:unless "?s a carrier:Error" .
```

## Data Flow

```
CarrierEvent (C callback via carrier_set_event_callback)
  → serialize to Turtle string (Rust, mirrors turtle_emit.c)
  → push to mpsc channel
  → main loop receives
  → evaluate before-insert DAG (guards → Lua exec → output inserted into Oxigraph)
  → insert original Turtle into Oxigraph
  → evaluate after-insert DAG (guards → Lua exec → output inserted into Oxigraph)
  → loop
```

## Module Details

### `main.rs`

CLI entry point with clap:

```
antenna --profile my.tox --store ./data --pipeline dag.ttl --nodes nodes.json
```

| Flag | Description |
|---|---|
| `--profile`, `-p` | Carrier .tox profile path (required) |
| `--store` | Oxigraph directory (omit for in-memory) |
| `--pipeline` | Turtle file with DAG definition to load at startup |
| `--nodes` | DHT bootstrap nodes JSON file |

Main loop:

1. `carrier.iterate()` — process Tox events
2. Drain Lua→carrier command channel, forward as `carrier_send_message()`
3. Drain event channel: for each Turtle string, run before-insert → insert → after-insert
4. Sleep for `carrier_iteration_interval()` ms

### `carrier.rs`

Manual FFI bindings matching `carrier.h`. Key types:

- `CarrierEventType` — `#[repr(C)]` enum, 30 variants matching C enum order exactly
- `CarrierEvent` — `#[repr(C)]` struct with type, timestamp, and union of data variants
- `CarrierEventData` — `#[repr(C)]` union of all event-specific structs
- `CarrierInstance` — safe wrapper holding `*mut Carrier` and a boxed `Sender<String>`

Event → Turtle serializer (`event_to_turtle`) ports `turtle_emit.c` to Rust:

- Each `CarrierEventType` variant → one-line Turtle statement
- Handles `is_turtle()` passthrough for messages that already contain carrier RDF (strips trailing dot, appends metadata)
- `turtle_escape()` for string literals (quotes, backslashes, newlines)
- `format_timestamp()` converts epoch ms to ISO 8601 without chrono dependency

The C callback (`event_callback`) is registered via `carrier_set_event_callback`. It receives `*mut c_void` pointing to a boxed `Sender<String>`, serializes the event, and sends through the channel.

Safe wrapper methods: `new()`, `iterate()`, `iteration_interval()`, `send_message()`, `get_id()`, `set_nick()`, `save()`.

Prefixes constant (`TURTLE_PREFIXES`) prepended to every Turtle document for parsing:

```turtle
@prefix carrier: <http://resonator.network/v2/carrier#> .
@prefix antenna: <http://resonator.network/v2/antenna#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
```

### `store.rs`

Thin wrapper around `oxigraph::store::Store`:

| Method | Description |
|---|---|
| `open(path)` | On-disk (path given) or in-memory store |
| `insert_turtle(turtle)` | Parse with `RdfFormat::Turtle`, load into default graph |
| `insert_turtle_to_graph(turtle, graph)` | Load into a specific named graph |
| `clear_graph(graph)` | Remove all triples in a named graph |
| `query(sparql)` | SPARQL SELECT, returns `QueryResults` |
| `ask(sparql)` | SPARQL ASK, returns `bool` |

All methods auto-prepend `TURTLE_PREFIXES` so standalone Turtle statements parse correctly.

### `pipeline.rs`

Loads the script DAG from Oxigraph via SPARQL, joining `antenna:Script` → `antenna:ScriptSource`:

```sparql
PREFIX antenna: <http://resonator.network/v2/antenna#>
SELECT ?script ?hook ?priority ?next ?source ?body ?creator ?signature ?where ?unless WHERE {
    ?script a antenna:Script ;
            antenna:hook ?hook ;
            antenna:scriptSource ?source .
    ?source antenna:body ?body .
    OPTIONAL { ?script antenna:priority ?priority }
    OPTIONAL { ?script antenna:next ?next }
    OPTIONAL { ?source antenna:creator ?creator }
    OPTIONAL { ?source antenna:signature ?signature }
    OPTIONAL { ?script antenna:where ?where }
    OPTIONAL { ?script antenna:unless ?unless }
}
```

`ScriptNode` struct holds: uri, hook, priority, body (Lua code), source_uri, creator, signature, next, where_clause, unless_clause.

Topological sort: find root nodes (no incoming `next` edges), walk chains, sort roots by priority. Orphan nodes appended at the end.

Execution: `run_before()` and `run_after()` walk the sorted chain. For each node:

1. Evaluate `where` guard (if set, `ASK` must return true)
2. Evaluate `unless` guard (if set, `ASK` must return false)
3. Execute Lua script via `LuaVm::exec_script(body, input)`
4. If script returns non-empty Turtle, insert it into Oxigraph
5. Chain output becomes next script's input

Guard evaluation for `before-insert` hooks inserts incoming data into temporary graph `<urn:antenna:pending>` so guards can match against data not yet in the store.

### `lua_vm.rs`

Sandboxed Lua 5.4 via mlua:

- Removes: `os.execute`, `os.remove`, `os.rename`, `os.tmpname`, `io.popen`, `io.open`, `io.close`, `io.input`, `io.output`, `io.read`, `io.write`, `io.lines`, `io.tmpfile`
- Keeps: `string`, `table`, `math`, `print`, `tostring`, `tonumber`, `type`, `pairs`, `ipairs`

Script contract:

```lua
-- Globals available:
--   input                          Turtle string (incoming RDF)
--   send_to_carrier(friend_id, text)   Send a command back to carrier
--
-- Return: Turtle string to insert into Oxigraph (or nil for nothing)
```

`exec_script(source, input)` sets the `input` global, evaluates the Lua source, reads the return value. mlua errors are converted to anyhow via `lua_err()` since `mlua::Error` doesn't implement `Send+Sync`.

`register_carrier_sender(tx)` exposes `send_to_carrier()` as a Lua global backed by an `mpsc::Sender<CarrierCommand>`.

## Error Handling

- **C FFI**: `carrier_new` returns NULL → anyhow error
- **Turtle parse errors**: logged to stderr, line skipped, processing continues
- **Lua errors**: logged to stderr, script skipped, chain continues
- **SPARQL guard errors**: logged to stderr, guard treated as failed (script skipped)
- **Oxigraph errors**: logged to stderr, processing continues
- **Carrier EOF**: main loop exits cleanly

## Usage

```sh
# Build
cargo build --release

# Run with in-memory store
antenna -p my.tox --pipeline dag.ttl --nodes nodes.json

# Run with persistent store
antenna -p my.tox --store ./antenna-data --pipeline dag.ttl --nodes nodes.json
```
