# BogKit

This repo contains some of the tooling we've been working on for building Bog style databases. We've collected these tools and examples in one cargo workspace, so you can start building immediately. 

The best way to create your project is the `bogkit` CLI, from the root of this repo:

```console
$ cargo run -p bogkit -- new [project-name]
```

This creates a new binary crate in `examples/[project-name]` with local path dependencies on the workspace crates. Two flavors:

- `--kind server` (the default) — a fold pipeline served over HTTP by `bog-serve`: writes, reads, search, and a live OpenAPI doc, all generated from the pipeline itself.
- `--kind embedded` — a plain Rust binary using fold directly (what `scripts/new-project.sh` used to produce; the script still works).

Run your project with:

```console
$ cargo run -p bogkit -- dev -p [project-name]
```

`dev` wraps `cargo run` with the bogkit conventions: a stable data dir in `~/.bogkit/data/[project-name]` (pass `--fresh` to wipe it) and `$PORT` (default 7877). For server projects, explore the API with:

```console
$ cargo run -p bogkit -- api          # pretty-prints the running server's /openapi.json
$ curl localhost:7877/views/total
$ curl -N localhost:7877/watch        # server-sent events, one per commit
```

## Bog Cloud infrastructure

This branch also contains [Bog Cloud](https://cloud.bog.new), an agent-first hosted prototype with HTTP, MCP, and a browser console. It runs on one shared Fly.io machine: a public server manages authentication, permissions, provisioning, and separate Bog worker processes. It does **not** provision Bogs through systemd or create a VM/container for each Bog.

```text
Browser / HTTP client / MCP agent
                |
         Bog Cloud server
  Authentication, permissions, registry
         and worker supervisor
                |
      Private Unix-socket connections
         /             |             \
     Bog A          Bog B          Bog C
     worker         worker         worker
       |              |              |
     Separate data directories on one persistent volume
```

### Provisioning and lifecycle

Creating a Bog checks workspace permissions, allowance, and available capacity. The service registers its identity and configuration, allocates storage, and starts a worker. Creation idempotency keys make retries safe without creating duplicate Bogs.

Each worker runs the platform's worker executable with the target Bog's data directory and definition. The application supervisor starts and monitors these processes directly. Idle workers stop after ten minutes; unused workers can also be reclaimed when another Bog needs capacity. A later request reopens the Bog from its saved data. Stopping a worker does not delete the Bog.

The current deployment configuration allows eight resident workers and two concurrent starts. An uncapped account or workspace removes its Bog-count allowance; it does not remove host capacity or per-Bog storage limits, and it does not purchase additional infrastructure.

### Isolation and its limits

- **Authorization:** the public server checks workspace membership and credential scope before routing operations. Single-Bog app credentials are restricted to their target Bog.
- **Processes:** each running Bog has a separate worker process and address space.
- **Storage and connections:** each Bog uses a separate data directory and private Unix socket rather than a publicly exposed worker port. Workers start with a cleared environment and receive only the configured worker variables.

This is **process separation, not a strong sandbox between tenants**. Workers share the host, operating-system user, and underlying volume. There are no separate per-Bog containers, filesystem namespaces, or individual CPU/memory budgets. A compromised worker could affect resources beyond its own Bog, and expensive workloads compete for shared host resources. The service runs trusted platform worker code; its process model should not be treated as a sandbox for arbitrary tenant executables.

### Persistence, supervision, and deployment

A SQLite registry under `/data/bog` stores management information. Bog data lives separately on the persistent Fly volume mounted at `/data`. The container entrypoint prepares the private application directory and drops privileges to the `bog` user before starting the server.

Fly's restart policy restarts the service when it exits; the application's supervisor manages individual Bog workers. There is no systemd dependency in this serving path.

As configured in this branch, the service uses one shared CPU and 1 GB RAM, with no automatic expansion. This is a single-host prototype, not a highly available fleet of independently isolated database servers. Deployment settings can change; the configuration below is the source of truth rather than a promise about supported user counts.

Implementation references:

- [Fly deployment configuration](deploy/bog-cloud/fly.toml)
- [Container image](deploy/bog-cloud/Dockerfile) and [entrypoint](deploy/bog-cloud/entrypoint.sh)
- [Worker supervisor](cloud/src/supervisor.rs) and [lifecycle defaults](cloud/src/config.rs)
- [Private worker client](cloud/src/worker_client.rs)
- [Service](cloud/src/service.rs), [registry](cloud/src/registry.rs), and [workspace permissions](cloud/src/workspace.rs)

## Documentation

- [Anatomy of a Bog](docs/anatomy-of-a-bog.md): definitions, resources, provisioning, and application access.
- [Composable Bogs](docs/bog-composable-resources.md): runnable examples and API details.

The fold crate is internally documented; to view the doc site, run:

```console
$ cargo doc --open -pfold 
```

## Hackathon submission

To enter the hackathon: fork this repo, build your project, then open a pull request against upstream. The PR is your official submission acknowledgment — be sure to fill which category you are submitting for in the PR template:

- agent support
- performance
- novel interface / gaming

Fill out the rest of the template (team, description, how to run) and you're good.

## In this workspace

### Fold
Fold is our take on an incremental programming framework, it's the engine that powers Bog. It’s a rust crate with iterator like primitives for materializing a stream of ever changing data into views. Statically typed and very, very fast.

### Embedded Static Embeddings (ESE)
ESE, our first take on a compiler oriented approach to static embedding. It’s a flattening of a tokenizer and map of embeddings into a perfect hash function. It’s also evidence that the approach is worth generalizing, and that there is much to be rethought about how embedding runtimes currently function.

### Approximate Nearest Neighbors... yeah (ANNy)
This is a very fast crate for creating HNSWs.

### bog-serve
Serve any fold pipeline over HTTP with the API generated from the pipeline itself: the input type describes the write routes (via schemars), the named terminal sinks describe the read routes, and the OpenAPI doc is assembled from the same values the router dispatches with — so it can't drift. Atomic batches, hybrid search, SSE watch streams, and custom routes included. See `serve/` and the crate rustdocs (`cargo doc --open -p bog-serve`).

### bogkit CLI
Scaffolding and a dev runner for BogKit projects (`cli/`): `bogkit new`, `bogkit dev`, `bogkit api`.

### Examples
In this directory you'll find a few examples that show bog style databases in various use cases.

- `starter` — the smallest possible fold database: a persistent count and bag, with inserts, reads, and retraction. `cargo run -p starter`
- `timeseries` — weather readings bucketed into hourly and daily aggregates, updated incrementally. `cargo run -p timeseries`
- `chat` — a chat backend where fold is the source of truth and every update is broadcast to clients over a websocket. `cargo run -p chat`, then open http://localhost:3000
- `search` — text search three ways over one document stream: BM25 keyword search, HNSW semantic search over ese embeddings, and hybrid rank fusion. A good base for agent memory or document search projects. `cargo run -p search`
- `search-server` — the search example served over HTTP by bog-serve: the same pipeline plus generated CRUD/search routes, a custom `/search/hybrid` fusion endpoint, and a live OpenAPI doc. `cargo run -p search-server`, then `curl localhost:7877/openapi.json`

## More about Bog
Bog is a database runtime that makes every attempt to do as much work as possible as early as possible, to make reads incredibly fast. This means compiling queries into functions that eagerly update their output as mutations occur.
