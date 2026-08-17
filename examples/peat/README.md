# peat

Agent memory as a fold. Coding agents deposit events — mechanical session
exhaust plus small judged observations — into one append-forever ledger, and
every readable surface is a [bogkit/fold](../../fold) view materialized
incrementally over it. Sessions end; what they learned does not.

```console
$ peat brief
== peat brief · 2026-08-16 ==

active in the last hour:
  bog-a-thon [b575bd9d] — <1h ago, 15 commits

current understanding (agent-asserted, newest wins):
  fold-hnsw (1 obs, <1h): fold 0.0.1 Hnsw strands old vectors on intra-tx
  upsert; patched in fork 640ff6f with red-proven regression
```

Integration is one settings block and one installed binary; a wake reads in
~0.13 s over a 44k-event ledger, and the brief stays a bounded read however
long the history grows. `capture` understands Claude Code transcripts and
Codex rollouts (format auto-detected, unknown formats rejected). Subagent
sessions do not fire the peat hooks — only top-level sessions deposit.

## Design

Two goals, asymmetric effort:

1. **Capture is sacred.** A session is recorded once or never; the ledger is
   the investment. The event schema is versioned, evolution is additive-only,
   and ingestion is idempotent and never-fatal.
2. **Everything else is disposable.** Views are replayable from the ledger,
   so any view, ranking, or rendering decision can be revised for free. The
   session-start prompt is a hot-editable template, never a pipeline change.

Three invariants hold everywhere:

- **No wall-clock reads in any fold path.** Time enters only at the
  capture/render boundary (event timestamps from transcripts or the caller;
  age labels at print time). This is what makes `asof` replay the truth of a
  past day rather than a reconstruction.
- **Additive schema evolution only.** Every envelope carries the
  `EVENT_VERSION` it was written under (currently 2); every envelope ever
  written must parse forever. New variants and optional fields only.
- **Every recalled line carries its disposition.** Age, origin kind, and
  citation status are printed inline — rank is not currency, and an uncited
  observation is visibly a bare assertion.

## Architecture

One `KeyedStream<EventId, Envelope>` carries every event. Each branch opens
with a `FilterMap` selecting the kinds it cares about (fold retracts whole
records, so hot event kinds must not share a record with expensive branches):

```
Envelope @ (session, seq)
 ├─ day buckets      → Aggregate("days")        → Table      per-day digest: tools, fails, commits, files
 ├─ file touches     → Multimap("file_sessions")             file ↔ session index
 ├─ searchable text  → Bm25("kw")                            keyword index: obs, said, user, final, compact
 │        └─ distilled only → ese → Hnsw("vec")              vector index: obs + final messages only
 │        └─ Table("texts")                                  hydration rows (kind, age, cited)
 ├─ observations     → Aggregate("subj") → Table("subjects") current understanding, newest wins
 │        └─ Multimap("evidence")                            full per-subject obs trail
 ├─ session rows     → Aggregate("sess") → Table             session summaries (span, cwd, branch, final)
 └─ ledger mirror    → Table("ledger")                       raw events, ordered — feeds asof and `events`
```

Everything is stock fold/ese/anny. One deliberate asymmetry: **vectors index
only distilled text** (observations and final messages). The firehose — user
messages, mid-session assistant messages, tool calls — stays BM25-only.
Embedding is the expensive lane; it is reserved for the text with the highest
signal density. Recall fuses both lanes with reciprocal-rank fusion.

## Event schema

`EventId = (SessionId, u32)`. The seq layout partitions three ranges:

| range | meaning |
|---|---|
| `line_index * 16 + block_index` | transcript-derived events (pure function of the transcript → idempotent re-capture) |
| `HOOK_FINAL_SEQ = (1 << 31) - 1` | the Stop hook's authoritative closing message |
| `OBS_SEQ_BASE = 1 << 31` and up | observations |

Event kinds, in trust order:

| kind | source | indexed |
|---|---|---|
| `SessionMeta` | transcript | — (pins cwd, branch, and ese model provenance) |
| `UserMsg` | transcript | BM25 |
| `ToolCall` | transcript | — (day digest) |
| `FileTouch` | Edit/Write tool calls | file↔session index |
| `Commit` | `git commit` tool calls | day digest, session rows |
| `Said` | substantive mid-session assistant messages (v2) | BM25 |
| `CompactSummary` | the compactor's own distillation (v2) | BM25 |
| `FinalMsg` | transcript tail, or Stop hook (authoritative) | BM25 + vector |
| `Compaction` | compaction markers | — |
| `Obs` | **the one judgment step** — an agent's recorded claim, with `derived_from` seqs citing the mechanical events it rests on | BM25 + vector, subjects, evidence |

Stored text is capped (`UserMsg` 2 KB, `FinalMsg` 8 KB, tool detail 500 B) at
char boundaries. `--json` output always carries full stored text; clipping is
display-only.

## Build

Part of the bogkit cargo workspace:

```console
$ cargo build -p peat        # first build downloads the ese model (build.rs) — slow once
$ cargo test  -p peat        # 8 tests; one #[ignore]d twin is SUPPOSED to fail when run
```

## Usage

The learnable surface is two verbs; everything else is reachable from their
output, because **every line of every read ends in the exact command that
looks one level deeper**:

```console
$ peat                      # orient: the brief
$ peat <thing>              # look closer — shape decides:
$ peat 2026-w33             #   a window (w33, 2026-07, 2026-08-14, q3, 2026, a..b)
$ peat 36f96b8d             #   a session (hex id prefix; + seq for one event)
$ peat fold hnsw fix        #   anything else: search (header names the reading)
$ peat obs <subj> "<claim>" # deposit one observation
```

The explicit subcommands below are the unambiguous spellings of the same
reads, and remain in `--help`.

### `peat capture <transcript.jsonl>` — ingest a session

```console
$ peat capture ~/.claude/projects/<slug>/<session>.jsonl
captured 161 events from session b575bd9d-…
```

Parses Claude Code transcript JSONL. **Unknown or unparseable lines are
skipped, never fatal** — capture must succeed on a transcript it has never
seen. Every event upserts by `(session, seq)`, so re-running the same
transcript is a no-op and re-running a grown transcript ingests only the
delta: this is the crash-recovery story. `--session <id>` supplies a session
id when the transcript lacks one; `--final-msg <text>` (passed by the Stop
hook from `last_assistant_message`) is authoritative over tail parsing, which
can lag at stop time.

### `peat obs <subject> <claim…>` — record one observation

```console
$ peat obs fold-hnsw "intra-tx upsert strands old vectors; fixed in 640ff6f" --from 1042
near subjects: fold-hnsw-perf (1 obs)
recorded → fold-hnsw (support 2)
```

The one judgment step — one short sentence (deposits over ~240 chars earn a
split-this nudge). `--from seq,seq` cites the mechanical events the claim
rests on; an uncited obs is displayed as a bare assertion everywhere it
appears. **Briefs clip; trails don't**: belief lines in the brief are an
index, truncated at ~120 chars and ending in `▸ peat <subject>`, which reads
the full newest-wins text and the complete evidence trail verbatim. Before writing, near-subject matches print as a drift guard. The
session id resolves from `--session`, else `.peat/current-session` (written
by the SessionStart hook). `--at YYYY-MM-DD` backdates for retroactive
annotation — `asof` briefs for that day will carry it.

### `peat brief [task words…]` — the session-start prompt

Renders in trust order: active sessions in the last hour, the per-day
digest, **the temporal ladder** ("further back"), the last session's closing
message, recently touched files, fused search hits (with `task words`), and
current understanding. `--json` emits the full structure.

The ladder is bounded reading over unbounded history: the rest of the past
as calendar bands that widen geometrically with distance (2 weeks, 2 months,
2 quarters, years, then one deep-past band), each an extractive digest
ending in its own descent handle. It is a pure read-time regrouping of the
materialized day table — nothing stored, nothing to go stale, `asof` gets it
for free — and `--budget N` (or `PEAT_BRIEF_BUDGET`, default 8) re-slices
without recomputing anything.

```text
further back:
  [w33 · aug 10–14] 3.7k tools (42 fail) · 123 commits · 6 sessions  ▸ peat 2026-w33
  [jul]             14k tools · 623 commits · 12 sessions            ▸ peat 2026-07
  [earlier · may 29 – aug 2] 22.9k tools · 878 commits               ▸ peat 2026-05-29..2026-08-02
```

### `peat zoom <window>` — descend

One window's digest, its children one rung finer (year → months → weeks →
days → sessions), and the accountable texts inside it: closing messages,
compaction summaries, observations. `peat <window>` is the short form.

### `peat recall <query…>` — search, hits only

Hybrid BM25 ⊕ HNSW recall with RRF fusion. Filters: `--kind
obs|said|user|final|compact`, `--since <days>`, `--session <prefix>`,
`--limit N`, or `--subject <name>` to read one subject's full evidence trail
instead of searching.

### `peat subjects` / `peat show <session> <seq>` / `peat events`

The claims register (every subject, newest-wins text, support count,
citation status); one event in full with the observations citing it; the raw
ledger oldest-first (auto-paged through `less -RFX` on a terminal). All take
`--json`.

### `peat asof <YYYY-MM-DD> [task words…]` — time travel

```console
$ peat asof 2026-07-10 formal model
== peat brief · 2026-07-10 · as of that day · 14075 events ==
```

Reads every ledger event at or before the end of that **local** calendar day
and folds the prefix through the *same* pipeline into a scratch database.
Ages are computed relative to that day. Replay determinism is oracle-tested,
which is why the result is the truth of that day and not a reconstruction.
~14k events replay in ~1.2 s.

## The database

- **Location**: `$PEAT_DB` if set, else `.peat/db` beside the nearest
  `.git`/`.jj` root above the working directory. `.peat/` belongs in
  `.gitignore`.
- **Single writer.** fold is single-writer and reads are exclusive too.
  Colliding invocations wait with backoff (default 120 s, tune with
  `PEAT_LOCK_WAIT_SECS`), then exit 75 (`EX_TEMPFAIL`) with an explanation —
  never a raw panic. A bulk capture can legitimately hold the lock for
  minutes; short hook invocations interleave transparently.
- **Durability**: bulk `capture` checkpoints the store afterward (memtable
  rotation) so subsequent opens do not replay a long journal; single-row
  writes deliberately do not, to keep the LSM's L0 healthy.
- Deleting `.peat/db` loses nothing that a re-capture of the transcripts
  cannot rebuild — except observations, which live only in the ledger. Back
  up the ledger, not the views.

## Claude Code integration

Hook contract, snippets, and caveats live in [`hooks/README.md`](hooks/README.md).
The shape:

| hook | does |
|---|---|
| `SessionStart` | writes `.peat/current-session`, runs `peat brief` — **stdout is injected into the session's context** |
| `Stop` | `peat capture` with `--final-msg` from `last_assistant_message`; then blocks **once** (guarded by `stop_hook_active`) asking the agent to deposit 1–3 observations while its context is still hot |
| `PreCompact` | salvage capture before the context window is replaced |
| `PostToolUse` (Bash) | on `git commit`/`jj describe`/`just land`, nudges the agent (via `additionalContext`) to deposit an obs |

Two contract facts worth repeating: hooks receive **stdin JSON** (there are
no `$CLAUDE_TRANSCRIPT_PATH`-style env vars), and every hook command must end
`|| true` — peat failing may never break a session.

## Multiple agents, one memory

Point every agent's hooks at one database:

```console
$ PEAT_DB=/path/to/shared/.peat/db peat brief
```

Each worktree/workspace keeps its own `.peat/current-session`; the ledger is
shared. Writers queue on the single-writer lock (proven under 14-process
load); the brief's *active in the last hour* section is the cross-agent
awareness surface, and *current understanding* interleaves every agent's
observations. Session exhaust from N agents becomes one mind.

## Output contract

**stdout is an API.** The SessionStart hook injects `brief` stdout verbatim
into an agent's context, and `--json` is machine-read. Therefore:

- spinners and phase timings live on stderr, only when stderr is a terminal;
- color reaches stdout only when stdout is a terminal, via one style
  vocabulary (bold headers, cyan identities, dim metadata, red distrust
  signals) applied identically by the template filters and every verb;
- piped, hooked, and `--json` output is byte-for-byte unstyled — no flags
  needed, `NO_COLOR` and `TERM=dumb` also respected.

## Customizing the brief

`brief.tmpl` (embedded default) is the experimentation surface: a minijinja
template over `BriefJson` with zero logic beyond section presence. Drop an
override at `.peat/brief.tmpl` and re-run `brief` — no rebuild. Style filters
available in templates: `h1`, `dim`, `warn`, `accent`, `clip(n)` (identity
when stdout is not a terminal).

## Testing

Two oracles are non-negotiable and written to be **red-capable** — each has a
proof it can fail:

1. **Retraction is observable**: upserting a revision over an event id makes
   the replaced text unfindable in both the keyword and vector indexes. An
   `#[ignore]`d twin asserts the opposite; `cargo test -p peat -- --ignored`
   must FAIL, proving the live oracle is not vacuous.
2. **Replay determinism**: folding any prefix of the ledger equals an
   independent scan's prediction, at multiple cut points, and transaction
   batching is unobservable.

Plus: idempotent double-capture, a golden test against a real (sanitized)
transcript in `tests/fixtures/`, never-fatal parsing of unknown line types,
and ISO-8601 round-tripping.

## Deferred by decision, not oversight

Belief support/flips semantics, `Merge{from,to}` subject-drift repair,
session fingerprints, a `why` verb over the evidence trail, multi-writer
spools. All are replay-backfillable later precisely because capture is total.
The subjects view stays deliberately dumb (newest-wins): anything cleverer
must be expressible as a fold over events still visible in the raw ledger.

## Performance

Measured on a ~44k-event ledger (28 sessions, including 100 MB+ transcripts):
`brief` 0.13 s · semantic query 0.18 s · full `asof` replay of 14k events
1.2 s. The work that made those numbers (journal checkpointing, lazy HNSW
recovery, per-key pending resolution in both search sinks) landed in `fold`
itself on this branch.
