# Soundings

**A live embedding-lens writing instrument.** The AI reads while you write — and writes only where you point, with the lenses checking its work.
Every edit is a fold delta; ese re-embeds the changed sentence in microseconds;
anny repositions it among the sentences of twenty Gutenberg classics; every
view on screen is an eagerly-maintained materialization, and the HUD prints
what the pipeline actually did — counted from inside it.

```bash
python3 scripts/prep_corpus.py     # once: 20 Gutenberg books -> data/canon.jsonl
cargo run -p soundings -- serve    # the instrument, http://localhost:4600
cargo run -p soundings             # embedding-quality probe (axis / drift gates)
cargo run -p soundings -- bench    # corpus-scale measurements
```

First `serve` builds the canon index (~20s at the default sampling; set
`SOUNDINGS_CANON_STEP=1` for all 129,097 sentences, ~3min once) and
checkpoints the HNSW graph — after that, cold start is instant. Kill the
process with `-9` mid-edit and relaunch: the document, its lenses, and the
canon are back before you can alt-tab.

## What it is, for a writer

- **Lenses, not opinions.** Sentences wear translucent washes from a
  concrete↔abstract axis (anchored with example *sentences* — the only way
  static embeddings hold an axis) and a length lens. The tool never suggests
  text; it shows you what your text is doing.
- **The corrigendum.** Deleting is the wrong primitive for composition —
  writers strike through, they don't erase. Retracting a sentence leaves it
  struck in correction crimson with a receipt of everything the cut took with
  it, and **restore** brings it back with its score bit-identical. You cut
  more freely when cutting isn't destruction.
- **Author your own lens.** Every draft negotiates its own axes. Give the
  instrument two sentences — one per pole — and it backfills a new lens over
  the whole document in one transaction, then maintains it on every
  keystroke. Pointing at examples is how writers think; parameters are not.
- **Gutter chips.** `len · 21st pct of canon` beside the sentence you just
  edited: a vague feeling ("runs short") becomes a calibrated fact against
  129,097 sentences of literature. Next to it, the microseconds the edit
  actually cost — proof the watching is free.
- **Nearest voices.** Each sentence's closest kin in the canon, by cosine —
  a margin of good company, refreshed only for the sentence you touched.
- **Precision rewrites.** Put your caret in a sentence and pull: "more
  concrete," "more abstract," or toward a specific canon voice from its
  nearest-voices card. A guest LLM drafts; the instrument audits — the
  proposal comes back with a receipt (`t 0.74 → 0.53 · VERIFIED`, or
  `cos-dist 0.959 → 0.915 toward Walden`), measured by the same ese axis
  and anny neighbors that painted the page. A draft that doesn't move the
  needle gets one retry with its own score as feedback; accepting is an
  ordinary edit — one fold upsert, every view updates. Optional: set
  `OPENAI_API_KEY` (the targeting and receipts are local either way).
- **The gale.** Press play: scripted edits at 60/s. The fold lane keeps every
  update; a naive lane running the *same scoring code* without deltas or an
  index falls seconds behind and drops most of its work. A referee hashes
  both lanes' scores at the same cursor and reports whether the fast answer
  is the same answer.

## How it uses bog-kit

| Surface | Mechanism |
|---|---|
| Doc stream | `KeyedStream<u32, String>` at `data/doc.db`: `Meter("keys") → (Table sents, Meter("embed") → Map(ese + axis) → Meter("scored") → Table scores)` — capturing-closure `Map`, scores as a materialized view |
| Canon stream | Second `KeyedStream` (separate thread): `Map(ese) → Hnsw<u32,f32,Cosine,512>` + `Table`, with the graph-snapshot fast-load so reopen never rebuilds |
| Corrigendum | fold's algebraic retraction: one `remove` un-happens every view; restore is a plain upsert — `t` returns bit-identical because ese is a pure function of the text |
| Authored lenses | Third stream: keys `(lens_id, sentence_id) → t`, one-wtx backfill, mirrored on every later edit — runtime structure as *data* on a compile-time-static operator graph; survives kill -9 |
| Length percentile | `Histogram` sink over all canon word counts — a materialized distribution, built once, constant-time per keystroke |
| HUD | the `Meter` operator (this PR): deltas counted and stages timed *inside* the pipeline; the wall-clock stopwatch is labelled as such |
| Voice cards | anny true deletion (retract a sentence, its card is released — no tombstones) · one HNSW query per edited key |

Framework changes in this PR, each free-standing: the **Meter** operator
(`fold::pipeline::Meter`, ~200 lines + tests); a **Bm25 correctness fix**
(same-transaction retract+insert corrupted changed term frequencies and
doc lengths — root-caused and fixed with last-write-wins set semantics,
test-pinned); and the **HNSW graph snapshot** from Séance (PR #6), adopted
with credit.

## Numbers (M-series MacBook, debug-opt profile)

| | |
|---|---|
| ese encode | ~291k sentences/s |
| one edit, end to end (retract → re-embed → re-score → repaint) | 15–130µs wall; 23–38µs inside the pipeline per the Meter |
| kNN against the canon | ~230µs |
| doc resume after `kill -9` | ~15ms |
| canon reopen (129,097 sentences) | 195s rebuild → **0.27s** snapshot fast-load |
| gale, 60 edits/s × 6s | fold **360/360** at median ~400µs · naive arm drops 75 (trial canon) to 347 (full canon) |

## Honesty notes

- The naive lane scopes the ANN index out (it linear-scans) — stated on
  screen; the comparison is maintenance strategy, not index vs no index.
- Voice agreement between HNSW and an exact scan is ~89% at the sampled
  canon and ~65% at the full 129k — approximate means approximate, and the
  referee reports it rather than hiding it.
- A drift / "doesn't sound like you" lens was prototyped and **cut**: static
  embedding geometry carries no authorial-register signal, and a lens that
  can't prove itself doesn't ship.
