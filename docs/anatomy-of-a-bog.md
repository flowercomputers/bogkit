# Anatomy of a Bog

An individual hosted Bog has a definition, stored records, derived resources, and a running worker. The definition describes how records become useful application operations. The Bog persists even when its worker is stopped.

For the surrounding hosting and isolation model, see [Bog Cloud infrastructure](../README.md#bog-cloud-infrastructure). For runnable examples and the detailed API contract, see [Composable Bogs](bog-composable-resources.md).

## The definition is the blueprint

This is a shortened version of the repository's [to-do definition](examples/composable/todo.json):

```json
{
  "version": 1,
  "input": "docs",
  "resources": {
    "docs": {
      "terminal": { "kind": "table" }
    },
    "open_count": {
      "stages": [
        {
          "kind": "filter",
          "expression": {
            "op": "equals",
            "field": "/completed",
            "value": false
          }
        }
      ],
      "terminal": { "kind": "count" }
    }
  },
  "expose": {
    "put": { "target": "docs", "action": "put" },
    "get": { "target": "docs", "action": "get" },
    "list": { "target": "docs", "action": "list" },
    "open_count": { "target": "open_count", "action": "read" }
  }
}
```

- `input` names the incoming collection of string-keyed JSON records.
- `resources` describes the tables, indexes, and calculations maintained from those records. Each resource can filter or reshape data before passing it to a terminal, which maintains the resulting table, aggregate, or index.
- `expose` declares which operations applications can use. A resource can exist internally without being exposed. This does not replace credential permission checks.

Fields such as `/completed` are JSON Pointers into a record. The definition contains behavior, not the application's records. A record arrives separately, under a key such as `task-123`:

```json
{
  "title": "Ship the dashboard",
  "completed": false,
  "priority": 3
}
```

Bogs created without an explicit definition use the built-in `records-v1` template. Configurable Bogs supply the more expressive blueprint shown above. Check authenticated `GET /v1/components` for availability, supported definitions, examples, and effective limits.

## Resources live inside the Bog

| Terminal | What it maintains |
| --- | --- |
| `table` | Records, optionally filtered or reshaped |
| `count` | A running count |
| `stats` | Numerical aggregates for a field |
| `ranked` | Records ordered by a numerical field |
| `bm25` | A full-text search index |
| `semantic` | A vector index for similarity search |

Each resource lives inside the same Bog worker and store. Adding a search resource does not launch another server or provision an external search service.

The runtime builds these resources using Fold's storage and data-processing components. Semantic resources generate embeddings with ESE and use an HNSW vector index backed by ANNy.

The configurable structure currently consists of one source feeding several branches. Each branch has its own filtering/projection stages and terminal; it is not an arbitrary graph of independently deployed services.

## Creation activates the blueprint

For a configurable Bog, the creation flow is:

```text
POST /v1/bogs: name + definition + stable idempotency key
                         |
       Validate definition, permissions, and limits
                         |
       Register identity, ownership, and definition
                         |
             Allocate its storage location
                         |
                Start its worker process
                         |
       Open storage and initialize declared resources
                         |
              Verify readiness: Bog is usable
```

Creation is asynchronous: acceptance is not readiness. Wait for ready status before using the Bog, and reuse the same idempotency key and request body when retrying creation.

The supervisor launches the existing worker executable with the Bog's data directory, definition file, revision, and private Unix socket. There is no per-Bog Rust compilation in this hosted path. The worker already contains the supported components; the JSON selects and connects them.

The runtime saves the definition alongside the data and checks its identity when reopening the store. It will not silently open existing data with an incompatible definition. Definition changes use the managed update flow described in the composable guide.

## Writes maintain the resources

Inserting the example `task-123` record stores the source record, updates the `docs` table, evaluates the `completed == false` filter, and increments `open_count`. The runtime checkpoints storage before reporting success.

Changing `completed` to `true` removes that record's contribution to the count. The application does not need to recount tasks or manually update a separate index.

The same mechanism feeds text and vector indexes. These changes happen through the Bog's write transaction, rather than an application-managed background indexing job. Limits are checked as part of the write path. A checkpoint failure is reported as an error and requires restart; it must not be interpreted as proof that the write did not persist.

## Applications use the platform gateway

```text
Application + single-Bog credential
                 |
Bog Cloud: authenticate and check permissions
                 |
         Private Unix socket
                 |
Bog worker: execute the permitted operation
                 |
Stored records / maintained resources
```

Applications discover exposed resources with `GET /v1/bogs/{bog_id}/resources` and use the corresponding resource query/search endpoints. Source writes use the record/batch APIs when the definition exposes those actions. Credential permissions and exposed operations both constrain access. The composable guide describes the equivalent MCP operations.

The worker is a child process managed by the cloud supervisor, not a separate systemd service or container for each Bog. When an idle worker stops, the definition and data remain on disk. A later data request can start the worker again and reopen the Bog, subject to host capacity.

There are therefore two distinct lifetimes: the Bog persists as an identity, definition, and store; its worker runs when needed to serve it.

## Implementation references

- [Definition schema and validation](../bog-definition/src/lib.rs)
- [Runtime storage, writes, and definition identity](../bog-runtime/src/lib.rs)
- [Resource construction and incremental updates](../bog-runtime/src/pipeline.rs)
- [Configured worker API adapter](../cloud-records/src/configured.rs)
- [Worker executable](../cloud-records/src/bin/bog-records-worker.rs)
- [Cloud worker supervisor](../cloud/src/supervisor.rs)
- [Hosted API contract](../cloud/src/contract.rs)
