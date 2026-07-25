# bog-bench

**Agent tooling churn as a live fold view.**

Category: **agent support**

Agents waste an enormous amount of their context on their own tooling — failed
`Bash` calls, retried edits, `Read`s that dump 40k characters back into the
window. You can measure that today, but only in batch: point a script at your
transcripts, wait, get a report, and the report is stale the moment an agent
makes another call.

bog-bench makes it a **materialized view instead of a report**. Every tool call
is a delta pushed through a `fold` pipeline into persistent sinks. Ingest a
session and the numbers move. Retract one and they roll back — exactly, to the
value they would have had if it never arrived. Nothing is ever recomputed.

## Run it

```console
$ cargo run -p bog-bench -- demo          # the whole story, no corpus needed
$ cargo run -p bog-bench -- recent 200    # against your own Claude transcripts
```

Each command is a **separate process**; the state between them lives on disk.

```console
$ cargo run -p bog-bench -- reset
$ cargo run -p bog-bench -- recent 200            # ingest the 200 newest sessions
$ cargo run -p bog-bench -- show
$ cargo run -p bog-bench -- retract <transcript>  # views roll back
```

Real output, 114 sessions of one author's Claude Code history:

```
2668 tool calls · 100 failed (3.7%)

  TOOL                        CALLS   FAIL   FAIL%      TOKENS      AVG
  Read                          416      3      1%     1447276     3479
  Bash                         1457     68      5%      534668      367
  Agent                          34      0      0%       53916     1586
  mcp__Claude_Browser__com…      10      5     50%       42221     4222
  Grep                           88      0      0%       30916      351

  churn — highest failure rates:
       50%  mcp__Claude_Browser__computer  (5/10 failed)
       33%  mcp__blume_improve__search_clusters  (1/3 failed)
       20%  mcp__Claude_Browser__javascript_tool  (1/5 failed)
```

`Bash` is called 3.5× more often than `Read`, but `Read` costs 2.7× more
context. And a browser MCP tool is failing half the time — the kind of churn
that is invisible until something counts it.

Then retract the single heaviest session and watch every view roll back:

```
retracted 69 calls
2599 tool calls · 97 failed (3.7%)
  Read     408   3   1%   862496   2114      ← was 416 / 1447276 / 3479
```

Exactly 584,780 tokens removed, the average recomputed, nothing rescanned.

## Why fold

This started as a port of an existing Python harness. Reading that harness
back is what made the case for Bog: its core module, `reducer.py`, describes
itself as *"incremental corpus aggregation — folds each session's result into
per-agent / per-tool counters and discards the call list."*

That is a hand-rolled, non-persistent, non-retracting reimplementation of what
`fold` does natively. Every section of the old report already had a primitive
waiting for it:

| Old harness (Python) | fold |
|---|---|
| per-tool counters in `reducer.py` | `KeyBy → Aggregate → Table` |
| failure / retry callouts | `Filter(!ok) → Count` |
| corpus totals | `Count` |
| `freeze.py` — 178 lines pinning a corpus for replay | deleted; persistence *is* the substrate |
| *no equivalent* | **retraction** — pull a session back out, every view rolls back |
| *no equivalent* | **append without rescan** — new sessions cost only their own deltas |

The last two rows are the point. A batch harness cannot do either at any price.

## What it measures

Per tool, maintained live: call count, failure count, failure rate, and context
cost (`result_chars / 4`, the convention the original harness used). Per
session: total calls and total context contributed — which is what makes
retraction legible, since you can watch one session's whole footprint leave.

## Design notes

Two things about `fold` that cost real time and are worth writing down:

**Returning `impl Push<D>` from a function is a trap.** It makes the pipeline's
`Reader` associated type opaque, and then `rtx` can no longer be destructured —
even inside the same crate. Both the pipeline constructor and the renderer here
are `macro_rules!` so the concrete type stays visible at each call site.
bogkit's own `timeseries` example solves it the same way.

**`Retain` is processing-time, not event-time.** Records are stamped with the
wall-clock time of the transaction that commits them, not with any timestamp
they carry. Ingesting a historical corpus therefore stamps everything "now", so
a rolling window over backfilled data needs `Retain::with_clock` and a replay
clock. Out of scope here; noted so the next person doesn't lose an hour to it.

## Scope

Deliberately small — this was built in a single hackathon session. In: Claude
Code transcripts, the tool-call join, per-tool and per-session views, ingest /
retract / show. Out: other agent runtimes, latency percentiles, the rolling
window, and anything requiring `anny` or `ese`.
