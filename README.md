# BogKit

This repo contains some of the tooling we've been working on for building Bog style databases. We've collected these tools and examples in one cargo workspace, so you can start building immediately. 

The best way to create your project is to run this terminal command in the root of this repo:

```console
$ ./scripts/new-project.sh [project-name]
``` 

This creates a new binary crate in `examples/[project-name]`, wires it into the workspace, and adds local path dependencies on `fold`, `anny`, and `ese` (though you may not necessarily use all of these).

Run your project with:

```console
$ cargo run -p [project-name]
```

## Documentation

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

### Sod
Symmetric replication for fold apps: local-first replicas ("sods") that
always accept writes and converge by exchanging hash-chained delta logs —
client↔server and p2p are the same protocol. Portable core (compiles to
wasm32) with fold as the default engine. See `sod/README.md` and the
`sod-demo` example.

### Examples
In this directory you'll find a few examples that show bog style databases in various use cases.

- `starter` — the smallest possible fold database: a persistent count and bag, with inserts, reads, and retraction. `cargo run -p starter`
- `timeseries` — weather readings bucketed into hourly and daily aggregates, updated incrementally. `cargo run -p timeseries`
- `chat` — a chat backend where fold is the source of truth and every update is broadcast to clients over a websocket. `cargo run -p chat`, then open http://localhost:3000
- `search` — text search three ways over one document stream: BM25 keyword search, HNSW semantic search over ese embeddings, and hybrid rank fusion. A good base for agent memory or document search projects. `cargo run -p search`
- `sod-demo` — a replicated notes bag: two or more local sod replicas (native CLI and a Node.js addon) converging over websocket sync. `cargo run -p sod-demo -- <dir> add hello`
- `sod-web` — the three-bog demo: a Next.js emoji reaction board where each instance embeds a sod replica; a Fly-deployed hub (live at https://sod-web-demo.fly.dev) plus two local instances survive a real wifi partition and converge on heal. See `examples/sod-web/README.md`

## More about Bog
Bog is a database runtime that makes every attempt to do as much work as possible as early as possible, to make reads incredibly fast. This means compiling queries into functions that eagerly update their output as mutations occur.
