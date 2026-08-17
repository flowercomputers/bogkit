# Parallax — project summary

**Bogathon 3 · August 16, 2026**

Parallax is an experiment in **goal-conditioned, multi-model retrieval over
mutable data**. Its starting premise is that an embedding model is not a
neutral map of reality: each model encodes a particular opinion about which
things are similar. The best retrieval model may therefore depend on what the
person or agent is trying to do.

The short version of what we learned:

- ESE was about **20× faster** than Potion Code at embedding this corpus.
- Potion Code was substantially better on the small Alinery evaluation,
  winning **9 of 10 query-goal cases**.
- The two models chose different top results for **4 of 5 topics**.
- One unchanged question produced a defensible model-winner reversal when the
  retrieval goal changed.
- Using fast ESE as a gateway to slower Potion looked attractive, but discarded
  too many of Potion's best results. We rejected the cascade.
- Bog/Fold can maintain both indexes from one stream and retract stale source
  data from both, so Parallax keeps parallel model views and chooses or fuses
  them at query time.

This is promising evidence for the idea, not proof that there is no universally
best embedding model. The current evaluation is one repository, five questions,
two goals, and ten judged cases.

## Why we started

Flower describes ESE as an exceptionally fast static embedding model. That
raised a deeper database question:

> If an embedding model defines what “similar” means, how can a database assume
> that one model—or even one notion of similarity—will remain correct forever?

The first idea was to index the same material with several models and let a
query choose weights such as:

```text
score(document) = a × model A + b × model B + c × model C
```

There is an important correction to that sketch: vectors and raw cosine scores
from unrelated model spaces are not directly comparable. Parallax keeps each
space separate and combines **ranks**, currently with weighted reciprocal-rank
fusion:

```text
score(document) = Σ weight(model, goal) / (K + rank(model, document))
```

The weights represent an explicit retrieval goal. They are visible policy, not
a learned router pretending to know the user's intent.

## Working thesis

**Compile models early; choose the retrieval goal late.**

Parallax treats embeddings as replaceable, model-specific materialized views
over authoritative source records. Bog maintains the views as records change.
At query time, the caller supplies both a question and a goal, and Parallax can
choose or fuse the views without mixing their vector spaces.

The stronger systems framing is:

> Parallax is not a new routing algorithm. It is an experiment in using a
> mutable database runtime to keep a mixture of retrievers current.

## What exists today

The runnable Rust example lives in `examples/parallax` and uses two genuinely
different learned embedding models:

| Model | Representation | Role in the experiment |
|---|---:|---|
| ESE `static-retrieval-mrl-en-v1` | 512 dimensions | Very fast general retrieval |
| `minishlab/potion-code-16M-v2` | 256 dimensions | Code-oriented Model2Vec retrieval |

The architecture is deliberately small:

```text
source files → line-preserving chunks → Fold KeyedStream
                                      ├─ ESE Map → HNSW 512d
                                      ├─ Potion Map → HNSW 256d
                                      └─ source Table

question + explicit goal → search each model → weighted rank fusion
```

The evaluation uses exact vector scans so approximate-nearest-neighbor behavior
cannot confound the model comparison. The final Bog demo separately proves that
the same corpus can be maintained in two HNSW views and a source table.

### Corpus

The measured snapshot contained:

| Item | Count |
|---|---:|
| Source files | 879 |
| Line-preserving chunks | 5,512 |
| Source bytes | 8.98 MB |

Included material:

- task and artifact Markdown;
- project documentation;
- Rust, TypeScript, and TSX source;
- shell source guards.

Excluded material:

- session scrollbacks and session JSON;
- attachments and worktrees;
- dependencies, generated files, and build output.

The source corpus remains in the local Alinery checkout. It is read-only and is
not copied into the Parallax repository.

### Frozen evaluation

Five topics are each asked under two goals:

1. sessions surviving app quit;
2. repository ownership across two app windows;
3. daemon compatibility and reuse;
4. destructive-confirmation safety;
5. read-only terminal history.

The two goals are:

- **Locate implementation** — retrieve the current code or executable rule.
- **Explain rationale** — retrieve the document that best explains why the
  behavior exists.

Candidates were pooled from both models' top five results. Two independent
agent raters scored each candidate blind on a 0–2 relevance scale for each
goal. When they disagreed, the evaluation keeps the lower grade. Metrics include
nDCG@5, MRR@5, Recall@5, encoding time, and warm exact-query latency.
