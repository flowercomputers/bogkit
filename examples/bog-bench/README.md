# bog-bench

**A benchmark harness for agent tooling, built so that staying current costs the
same no matter how much history you have.**

Category: **performance**

An agent's context window is its scarcest resource, and most of what fills it is
not the user's problem — it is the agent's own tooling. A `Read` that dumps 40k
characters back. A `Bash` call that fails and gets retried. An MCP tool quietly
erroring half the time and burning a turn on every attempt.

Harnesses that measure this run in batch: point a script at your transcripts,
re-read all of them, recompute every total, emit a report — which is stale the
moment an agent makes another call, and gets slower every week you keep using
it.

bog-bench is built on `fold` instead, and the headline number is what that
buys: **16 ms to fold in a new session, whether your history is 51 sessions or
801**, against 417–630 ms and climbing for a full rescan. Both routes are
checked against each other before either number is reported.

bog-bench makes it a **materialized view instead of a report**. Every tool call
is a delta pushed through a `fold` pipeline into persistent sinks. Ingest a
session and the numbers move. Retract one and they roll back — exactly, to the
value they would have had if it never arrived. Nothing is ever recomputed.

## What "cost" means here, precisely

The leaderboard ranks on **characters of tool-result payload**, because that is
the one quantity measurable exactly for every call. A `~TOK` column shows the
conventional `chars / 4` estimate beside it, clearly marked as an estimate.

It would be nicer to report real token usage, and the transcripts do carry it —
but it cannot be attributed per call. Prompt caching drives `input_tokens` to
near nothing (a typical turn: `input_tokens: 6` against `cache_read: 25803`), so
a tool's true marginal cost is only recoverable by diffing total context between
consecutive turns, and only for turns holding exactly one tool call. That covers
a fraction of calls and would silently bias the ranking toward whichever tools
happen to get called alone.

So: characters are measured and ranked on, tokens are estimated and labelled.
The estimate holds for ASCII-ish code and prose; it undercounts CJK (closer to
one token per character) and punctuation-dense JSON.

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

  TOOL                        CALLS   FAIL   FAIL%       CHARS     ~TOK
  Read                          416      3      1%     5789104  1447276
  Bash                         1457     68      5%     2138672   534668
  Agent                          34      0      0%      215664    53916
  mcp__Claude_Browser__com…      10      5     50%      168884    42221
  Grep                           88      0      0%      123664    30916

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
  Read      408   3   1%   3449984   862496     ← was 416 / 5789104 / 1447276
```

Exactly 2,339,120 characters removed, every derived figure recomputed, nothing
rescanned.

## The benchmark

```console
$ cargo run -p bog-bench -- bench 800
```

Two arms, both timed end-to-end (parse *and* fold, because a batch tool really
does re-read every transcript), and **checked against each other before either
is reported** — a faster wrong answer is not an answer.

- **incremental** — a new session arrives; fold it into the corpus already on disk.
- **rescan** — recompute the whole corpus from nothing, as a batch harness must.

| corpus | incremental | rescan | rescan parse | rescan fold |
|---|---|---|---|---|
| 51 sessions | 16.7 ms | 417 ms | 46 ms | 372 ms |
| 201 sessions | 33.8 ms | 550 ms | 183 ms | 367 ms |
| 801 sessions | 16.0 ms | 630 ms | 266 ms | 364 ms |

Both arms agree on the call count at every size.

The headline ratio (16–40×) is noisy, because the arriving session is whichever
is newest and its size varies run to run. **The durable result is the shape, not
the ratio:** incremental cost is flat in corpus size — 16.7 ms across 51
sessions, 16.0 ms across 801 — while rescan climbs with every session added.
Rescan's fold time stays near 365 ms regardless of corpus size, so that arm is
dominated by fixed LSM setup rather than per-call work; the growth is in parsing.

That flatness is the whole argument for putting this on a CI gate. Checking
whether a change caused agent churn costs the same whether your history is a
week old or a year old.

## Rolling window

A CI gate does not want "churn since the beginning of time", it wants "churn
lately". `Retain` gives that as one operator:

```console
$ cargo run -p bog-bench -- window 24 400     # last 24h, over 400 sessions
```

```
replayed 2926 calls spanning 269h · window = last 24h
1391 calls still inside the window
```

| horizon | calls in window |
|---|---|
| 1 h | 324 |
| 6 h | 517 |
| 24 h | 1 392 |
| 72 h | 1 689 |
| 336 h | 2 927 (whole corpus — the span is only 269 h) |

**The catch, and the workaround.** `Retain` is *processing-time*: it stamps each
record with the wall clock of the transaction that commits it and ignores any
timestamp the record carries. Replaying history through it naively would stamp a
year of transcripts as all arriving "now", and every window would return
everything. `Retain::with_clock` is the way out — bog-bench drives a synthetic
clock from the transcripts' own timestamps and commits in event order, one
transaction per hour of corpus time, so expiry follows event time.

This runs on its own stream and its own database, deliberately: adding a node to
the main pipeline changes its keyspaces, and a secondary view is not worth
risking the primary ones.

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
cost in measured characters (plus the labelled token estimate). Per
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
