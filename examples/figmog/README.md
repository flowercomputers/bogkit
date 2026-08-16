# figmog

A fold-backed local mirror of one Figma file: `pull` fetches it once and
keeps materialized indexes in a fold database, so every read after that —
search, tree walks, component/style/variable queries — answers from local
storage in milliseconds, spending zero Figma API calls and hitting zero
rate limits.

## Quick start

```console
$ export FIGMA_TOKEN=figd_…            # figma.com → settings → security → personal access tokens
$ cargo run -p figmog -- pull "https://www.figma.com/design/<key>/<name>"
$ cargo run -p figmog -- search "pricing card"
$ cargo run -p figmog -- watch          # keep it fresh in another terminal
```

After the first `pull`, figmog remembers the file key (in `.figmog/current`
under the current directory), so every later command — including `watch`
and all the read commands below — can drop the file argument.

## Commands

Read commands never touch the network: they open the local store and read
one snapshot. `--json` (global) emits machine-readable JSON on stdout
instead of the human-readable format; `--db <path>` (global) overrides the
store location (default `.figmog/<file-key>/db`).

| command | reads | behavior |
|---|---|---|
| `figmog pull [file] [--from-file <json>] [--fresh]` | — | sync now; prints a churn summary (`+added ~changed -removed`). `file` is optional after the first pull. `--from-file` ingests a saved `GET /v1/files/:key` response instead of the network (offline ingestion, and what keeps the CLI tests hermetic). `--fresh` wipes the store and rebuilds from scratch. |
| `figmog watch [file] [--interval N]` | — | poll loop: cheap metadata check every `N` seconds (default 10), full pull only on an actual change |
| `figmog status` | meta + nodes | file name, version, last modified, node count |
| `figmog pages` | by_type + nodes | list CANVAS pages (id, name) |
| `figmog tree [id] [--depth N]` | children + nodes (+ by_type to find the root) | indented outline: `name  [type]  id`; root defaults to the DOCUMENT node |
| `figmog get <id> [--children]` | nodes (+ children) | the full `raw` JSON of a node; `--children` inlines one level of child summaries |
| `figmog find --type <TYPE> [--page <id>]` | by_type + nodes | nodes by type, optional page filter |
| `figmog search <query> [-n N]` | Bm25 + nodes | ranked hits (default 10): score, id, type, name, page, text snippet |
| `figmog instances <id\|key\|name>` | nodes + instances_of + components + component_sets | resolve the argument to a component (by node id, global key, or a unique component/component-set name — a set name expands to all its variants), list instance nodes |
| `figmog components` | components + component_sets + nodes | design-system inventory: component sets with their variant axes, standalone components |
| `figmog styles [--type <t>] [--values]` | styles + styled_by (+ nodes) | styles with usage counts; `--values` derives each style's definition from a consumer node (§ below) |
| `figmog uses <id>` | styled_by / bound_to + nodes | nodes using a style id or bound to a variable id |
| `figmog vars [id]` | nodes + variables + variable_collections | variables: authoritative record if imported, else inferred value(s) + binding sites |
| `figmog import-variables <path>` | — | upsert variable/collection records from a variables export (see "Variables on a free plan") |

Node ids accept both `12:34` and `12-34` forms everywhere. Auth is a
personal access token from `FIGMA_TOKEN`. Since `pull`/`watch` are the only
commands that touch the network, everything else works fine with no token
set as long as a store already exists.

## How sync works

`watch` polls the cheap `GET /v1/files/:key/meta` endpoint (Tier 3) every
interval and only spends a Tier-1 `GET /v1/files/:key` fetch — the
expensive, rate-limited call — when the file's content-modification
watermark actually changes. Every fetch, whether from `pull` or `watch`,
flows through fold's `KeyedStream` upsert-diff: re-syncing a byte-identical
node is a no-op through the whole pipeline (zero graph churn, zero index
writes), so a spurious trigger or a repeated `pull` costs one Tier-1 fetch
and nothing else. Since the November 2025 rate-limit overhaul, file
endpoints are capped around **10 requests/min on the free (Starter)
plan**, and there is no delta API — this polling design is what makes
that budget workable for an agent that wants to treat the file as live.

## Variables on a free plan

The Variables REST endpoints (`variables/local`, `variables/published`)
are Enterprise-only, so figmog never calls them. Variables are supported
through two complementary paths:

**Path 1 — mirrored bindings + inference (always on, zero setup).** Every
variable-bound property in the file JSON carries a `boundVariables`
reference, and Figma bakes the resolved concrete value into the same node
next to it. figmog scans every node for these bindings at every depth and
inverts them into a `bound_to` index. `figmog vars` aggregates at read
time: for each variable id, every binding site (node + property path) and
the observed value(s) baked in there. This covers each variable's
**default-mode value**; values from a non-default mode appear only where a
frame explicitly overrides its mode.

**Path 2 — authoritative import (optional).** `figmog import-variables
<export.json>` upserts full-fidelity variable and collection records:
collections, modes (e.g. light/dark), per-mode values, descriptions,
scopes. It accepts two shapes: the Enterprise REST `variables/local`
response, or the JSON produced by the free-plan escape hatch below — the
Figma Plugin API can read local variables on **any** plan, run from
Figma's own developer console. `figmog vars` prefers an imported
(authoritative) record over inference whenever one exists.

```js
// Figma → Plugins → Development → Open console, then paste:
(async () => {
  const collections = await figma.variables.getLocalVariableCollectionsAsync();
  const variables = await figma.variables.getLocalVariablesAsync();
  const out = { variables: {}, variableCollections: {} };
  for (const c of collections)
    out.variableCollections[c.id] = { id: c.id, name: c.name, modes: c.modes, defaultModeId: c.defaultModeId };
  for (const v of variables)
    out.variables[v.id] = { id: v.id, name: v.name, resolvedType: v.resolvedType,
                            variableCollectionId: v.variableCollectionId,
                            valuesByMode: v.valuesByMode, description: v.description, scopes: v.scopes };
  console.log(JSON.stringify(out));
})();
// save the logged JSON, then: figmog import-variables vars.json
```

A third source — Figma's MCP servers, which expose `get_variable_defs` —
exists for paid seats only and is deliberately not built into figmog: the
desktop server needs a Dev/Full seat on a paid plan, the remote server
caps Starter users at 6 tool calls a *month*, and the tool is
selection-scoped rather than whole-collection. Anyone with a paid seat can
pipe its output into `import-variables` by hand; figmog itself never
depends on MCP.

## Manual live check

Not run in CI (needs a real `FIGMA_TOKEN` and a real file); this is how to
verify it by hand:

```console
$ export FIGMA_TOKEN=figd_…
$ cargo run -p figmog -- pull <figma url>
$ cargo run -p figmog -- status
$ cargo run -p figmog -- components
$ cargo run -p figmog -- search "pricing card"
$ cargo run -p figmog -- vars
```

`pull` pays the one Tier-1 fetch and prints a churn summary. Every command
after it — `status`, `components`, `search`, `vars` — is a local read:
acceptance is that they return in milliseconds, regardless of how large
the mirrored file is.

## Limitations

- **Variables** — inference (Path 1, always on) covers each variable's
  default-mode value; a non-default mode's value is visible only where a
  frame explicitly overrides that mode. Full per-mode fidelity requires
  `import-variables` (Path 2).
- **No image renders** — figmog mirrors document structure and properties,
  not rendered pixels; there's no `GET /v1/images` integration.
- **Style definitions are derived, not authoritative** — the file JSON's
  `styles` map is metadata only (id, name, type), not the style's actual
  properties. `figmog styles --values` derives a definition from one
  consumer node's resolved properties (e.g. a text style's `TypeStyle`
  from a TEXT node that uses it) — if a style currently has no consumers,
  it has no derivable value.
- **Change detection is polling, not webhooks** — `watch` polls the cheap
  `last_touched_at` metadata field on an interval; Figma's `FILE_UPDATE`
  webhook is unavailable on the free plan and debounced up to 30 minutes
  even where it exists, so polling a cheap Tier-3 endpoint is both
  simpler and faster.
- **Instance overrides beyond the serialized subtree are not resolved** —
  Figma serializes an INSTANCE's overridden children as ordinary nodes
  under it, and those mirror like any other node, but overrides that
  Figma doesn't materialize into the subtree are not reconstructed.
