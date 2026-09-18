# Bog as a Database Language, and Bog Cloud as Its Managed Runtime

## Purpose

Bog is intended to become a universal toolkit for implementing custom databases. Its long-term identity is not a hosted JSON store or a configurable database product. Bog is a programming language and compiler for describing specialized databases, assembling components such as Fold, ESE, Anny, storage engines, schemas, and transports into compact, high-performance artifacts that can be embedded or deployed.

Bog Cloud should abstract that intended experience:

```text
write or select a Bog program
-> compile it
-> provision an instance
-> receive a generated API
```

The existing `records-v1` service is therefore one precompiled Bog program and compatibility template. It is not the definition of what a Bog can be.

## System model

```text
Bog source program
        |
        v
Bog compiler
  type checking
  resource planning
  capability analysis
  API derivation
        |
        v
Bog intermediate representation
  inputs
  Fold graph
  storage layout
  ESE encoders
  Anny indexes
  transactions
  exposed operations
        |
        v
Compiled database artifact
  embedded library
  local executable
  server process
  WebAssembly module
  cloud worker
        |
        v
Generated interfaces
  native API
  HTTP
  MCP
  client library
  schema and documentation
```

A Bog is a compiled database program: its authoritative inputs, incremental computation graph, durable and rebuildable resources, transaction boundaries, and explicitly exposed operations.

## Example Bog program

A future todo database might be expressed as:

```bog
database Todos {
  input todos: keyed<string, Todo>

  resource all = table(todos)
  resource total = count(todos)

  resource open =
    todos
    |> filter(.completed == false)
    |> table()

  resource by_due_date =
    todos
    |> filter(.due_at != null)
    |> ranked(by: .due_at, ascending)

  resource text =
    todos
    |> text_search(
         fields: [
           field(.title, weight: 3),
           field(.notes, weight: 1)
         ]
       )

  resource semantic =
    todos
    |> map(concat(.title, " ", .notes))
    |> encode(ese.v1)
    |> nearest_neighbors(anny.cosine)

  expose mutation put_todo = todos.put
  expose mutation remove_todo = todos.remove

  expose query get_todo = all.get
  expose query list_open = open.list
  expose query upcoming = by_due_date.top
  expose query search = text.search
  expose query related = semantic.search
}
```

The program describes accepted inputs, the Fold computation graph, materialized resources, transactional relationships, and the public operations from which Bog derives native, HTTP, MCP, documentation, and client interfaces.

Internal resources need not be exposed. The database author controls the public abstraction.

## Source language and manifest distinction

The user-authored representation should be a Bog program. A normalized manifest remains valuable, but it should primarily be compiler output: the Bog intermediate representation and deployment plan.

For example, this source expression:

```bog
resource open = todos |> filter(.completed == false) |> table()
```

might compile to:

```json
{
  "resource": "open",
  "source": "todos",
  "operators": [
    {
      "kind": "filter",
      "expression": {
        "equals": ["$.completed", false]
      }
    }
  ],
  "terminal": {
    "kind": "table"
  }
}
```

Bog Cloud can inspect this representation to estimate resources, validate host capabilities, generate interfaces, plan migrations, explain the database, and provision its runtime. Alternative frontends such as a visual builder, JSON declaration, Rust macro, or agent-generated program can compile into the same IR.

## Architectural layers

### 1. Bog language

The user-facing language should eventually cover:

- Data types and schemas
- Keyed and append-only inputs
- Mutations and transaction boundaries
- Fold pipelines and terminal resources
- Materialized tables, indexes, aggregates, and rankings
- Text and semantic search
- Public and private operations
- Resource hints and limits
- Modules, packages, and composition

It should feel like writing a small database program rather than configuring a hosted database.

### 2. Intermediate representation

The compiler emits a stable normalized representation that is:

- Versioned and deterministic
- Fully typed
- Independent of surface syntax
- Inspectable without executing tenant code
- Suitable for capability and resource analysis
- Serializable for local and cloud deployment
- Stable enough to support migration planning

### 3. Compiler and planner

The compiler resolves a program into concrete components:

- Fold streams, operators, and terminal sinks
- Fjall-backed authoritative and derived storage
- ESE encoders and their versions and dimensions
- Anny index types, metrics, and parameters
- Mutation and query boundaries
- Synchronous and asynchronous resource semantics
- Generated API contracts
- Backup, restore, and rebuild requirements

The compiler should reject invalid programs before deployment. Examples include mismatched vector dimensions, invalid public references to private resources, supposedly atomic mutations that depend on asynchronous work, and incompatible storage-layout changes.

### 4. Runtime and targets

The same Bog program should eventually support several targets:

| Target | Result |
| --- | --- |
| Embedded Rust | A small library linked into an application |
| Native local | A standalone database executable |
| HTTP server | A self-hosted service with generated routes |
| MCP server | An agent-facing database |
| WebAssembly | A constrained portable database |
| Bog Cloud | A managed instance with identity and lifecycle |

The compiler should report target compatibility rather than silently changing database semantics.

### 5. Bog Cloud control plane

Bog Cloud manages:

- Source, package, IR, or artifact submission
- Compilation and compiler diagnostics
- Artifact storage and instance provisioning
- Database versions and deployments
- Credentials, accounts, and workspaces
- Worker lifecycle and capacity
- Generated HTTP and MCP endpoints
- Usage and resource readiness
- Backfills, rebuilds, and migrations
- Operational logs and observability

Bog Cloud does not define database semantics independently. The Bog program and compiler do.

## Resource vocabulary

Bog should incrementally expose a standard library of database resources:

| Resource | Purpose | Likely component |
| --- | --- | --- |
| `table` | Durable keyed records | Fold table |
| `log` | Ordered append-only events | Fold stream |
| `count` | Incremental cardinality | Fold count |
| `stats` | Min, max, sum, and average | Fold statistics |
| `histogram` | Bucketed aggregation | Fold histogram |
| `index` | Records organized by a derived key | Keyed Fold pipeline |
| `ranked` | Top or bottom records by score | Fold ranked terminal |
| `text_search` | Keyword relevance search | Fold BM25 |
| `vector_search` | Nearest-neighbor search | ESE plus Anny/HNSW |
| `projection` | A derived record shape | Fold mapping |
| `filter` | A materialized subset | Fold filtering or retention |
| `relation` | A derived lookup across inputs | Later-phase composition |

ESE and Anny should become typed language-level database resources, not opaque cloud add-ons. The compiler needs to understand encoder identity, output dimensions, scalar type, distance metric, rebuild behavior, storage cost, replacement semantics, and target support.

## Generated interfaces

Explicitly exposed operations should produce one shared contract for:

- Embedded functions
- HTTP routes
- MCP tools and resources
- OpenAPI and JSON Schema
- Human and machine documentation
- Typed client methods

For example:

```bog
expose query list_open = open.list
```

should generate equivalent interfaces without Bog Cloud separately interpreting the resource graph.

For generic clients, a stable discovery and invocation family is preferable to generating an unbounded number of MCP tools:

- `list_inputs`
- `describe_input`
- `mutate_input`
- `get_input_record`
- `list_resources`
- `describe_resource`
- `query_resource`
- `search_resource`
- `wait_for_resource_change`

Schemas and resource metadata specialize these operations for each compiled database.

## Consistency model

The compiler and runtime should make consistency explicit:

- A source mutation and its synchronous Fold-derived resources commit atomically.
- A successful mutation response means the authoritative write is durable.
- Synchronous resource reads identify the source sequence they represent.
- Expensive resources may be asynchronous only when declared as such.
- Asynchronous resources expose their indexed-through sequence and staleness.
- A failed transformation cannot partially commit a mutation.
- Restore either restores a compatible derived resource or deterministically rebuilds it.
- Restart-aware change cursors remain distinct from replayable log positions.

An asynchronous semantic index could report:

```json
{
  "items": [],
  "source_seq": 921,
  "indexed_through_seq": 917,
  "stale": true
}
```

## Compilation and deployment lifecycle

Cloud compilation and instance creation should be separable:

```text
compile once
-> inspect and test the artifact
-> instantiate it many times
```

Bog Cloud should eventually accept:

- Bog source
- A compiled Bog IR
- A published Bog package or template
- A precompiled artifact

A database instance records:

- Source and package revision
- Compiler and IR version
- Artifact digest
- Storage-layout version
- Active API contract
- Resource readiness and indexed-through positions

Program changes follow a deployment lifecycle:

```text
source revision
-> compilation
-> migration plan
-> deploy
-> backfill or rebuild
-> activate
```

The compiler should classify changes as compatible, rebuild-required, backfill-required, destructive, or unsupported. Adding an index should not make authoritative source data unavailable while the index is building.

## Incremental roadmap

### Phase 1: make `records-v1` a canonical Bog program

Express the current cloud database as the first Bog program:

```bog
database Records {
  input docs: keyed<string, JsonObject>

  resource docs = table(docs)
  resource total = count(docs)

  expose docs.get
  expose docs.put
  expose docs.remove
  expose docs.list
  expose total.read
}
```

It can initially compile to the exact Rust construction used today. Existing APIs and data remain compatible.

### Phase 2: define the minimal Bog IR

Represent keyed inputs, JSON and named record types, tables, counts, CRUD operations, bounded reads, durability policy, and logical storage limits. Begin with faithful translation rather than optimization.

### Phase 3: generate the cloud worker from compiled definitions

Move `records-v1` knowledge out of the cloud worker. Initially, a trusted runtime may interpret Bog IR. Later targets can use native code generation, WebAssembly, or precompiled components.

### Phase 4: derive APIs from exposed operations

Generate native, HTTP, MCP, schema, documentation, and client contracts from the same compiler output. Publish manifest, resource, operation, and target-capability discovery.

### Phase 5: expose more Fold constructs

Add map, filter, keying, projection, retention, counts, statistics, histograms, rankings, grouping, multiple inputs, and pipeline composition. Each feature must pass through syntax, type checking, IR, runtime compilation, persistence, API generation, backup and restore, and both local and cloud conformance tests.

### Phase 6: add reusable Bog packages

Allow immutable, versioned database programs to be published as templates. Start with workspace-private packages, descriptions, compatibility declarations, cost estimates, export, import, and deprecation without breaking existing instances.

### Phase 7: add BM25 text search

Expose Fold's text-search capabilities with pinned tokenization, normalization, weighting, bounded results, deterministic restore behavior, and correct replacement and deletion semantics.

### Phase 8: add ESE and Anny semantic search

Support server-managed, versioned ESE encoders and typed Anny indexes. Bound dimensions, concurrency, indexed records, and query size. Never mix embeddings from different encoder versions. Support deterministic rebuilding and direct-vector queries only when dimensions match exactly.

### Phase 9: add append-only logs

Support event-oriented programs with server-assigned positions, bounded reads after a cursor, retention, durable resumption, and Fold-derived tables and aggregates. Keep durable log positions distinct from notification cursors.

### Phase 10: add migrations and backfills

Compile database revisions into explainable migration plans. Track resource build state, preserve old resources during compatible rebuilds where practical, and require explicit handling for destructive layout changes.

### Phase 11: controlled extension mechanisms

Only after declarative Bog programs prove insufficient, consider Flower-authored modules, reviewed workspace modules, and sandboxed WebAssembly transformations. General uploaded native code should remain distant because it greatly expands the security and operational boundary.

## Cloud experience levels

Bog Cloud should meet users and agents at several levels:

1. **Ready-made database:** create a basic records Bog without language knowledge.
2. **Intent-driven database:** describe an application and let an agent author or select a Bog program, explain it, compile it, and provision it.
3. **Direct Bog authoring:** write, compile, inspect, and deploy Bog source.
4. **Portable build:** build the same program for embedded, native, WebAssembly, self-hosted, or cloud targets.

The hosted product is an accessible facade over the database language, not a parallel database product that happens to share its implementation crates.

## Resource accounting

Source records alone do not describe the cost of derived databases. Usage should distinguish:

```json
{
  "source_data_bytes": 740112,
  "derived_data_bytes": 1319014,
  "vector_index_bytes": 4150021,
  "resource_count": 6,
  "embedding_operations_this_period": 220
}
```

Prototype limits can cover source bytes, derived bytes, resource count, indexed records, vector dimensions, active backfills, worker residency, and host-wide encoder capacity. An uncapped Bog-count workspace remains subject to per-Bog and host-wide resource limits.

## North-star acceptance test

1. An agent writes a small Bog program for a todo application.
2. Bog compiles it and explains the resulting Fold, ESE, and Anny resources.
3. The program runs locally as a small embedded or standalone database.
4. The same program deploys to Bog Cloud.
5. The cloud instance exposes generated HTTP and MCP interfaces.
6. Local and cloud instances pass the same semantic conformance suite.
7. The application can move between them without redesigning its data model.
8. The author adds an index or query by editing the Bog program and reviewing a compiler-generated migration plan.

At that point, Bog Cloud demonstrates Bog's intended identity: a language that turns compact database programs into specialized, high-performance databases wherever they need to run.
