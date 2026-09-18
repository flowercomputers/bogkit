# Exact Cowork and Scatter search reproduction

Local source verification on September 18, 2026. This is not a production retest.

Run `cargo test -p bog-runtime --test search_reproduction -- --nocapture`.
The test passed using the actual built-in encoder and maintained indexes. It compares each returned similarity score with a direct cosine calculation on the exact `/text` string, within 0.00001. User names and timestamps do not enter the embeddings.

## Exact inputs

Semantic and BM25 resources both extract `/text`:

```json
[
  {"key":"m1","user":"ed","text":"anyone want to grab ramen tonight","ts":1},
  {"key":"m2","user":"ash","text":"the shop was slammed today, new jackets sold out","ts":2},
  {"key":"m3","user":"ed","text":"deploying the database fix now","ts":3}
]
```

| Query | Returned semantic order with score (1 − cosine distance) | BM25 |
| --- | --- | --- |
| dinner plans | m3: 0.04537284; m2: 0.02076328; m1: −0.02440715 | empty |
| noodles for dinner | m2: 0.06128788; m1: 0.06036597; m3: 0.03365594 | empty |

The poor relevance is reproducible. The intended ramen message does not rank first for either query. The three records are indexed, and direct encoding agrees with index distances, so this fixture points to encoder retrieval quality rather than stale indexing or the wrong field. No minimum corpus size is inferred or required by the implementation. Returning nearest candidates is not evidence that those candidates are relevant.

The Scatter comparison uses one record with title `A cabin for deep work` and body `An isolated shelter among trees, away from interruptions.`, extracting `/title` and `/body` joined with a newline. Query `quiet woodland retreat` returns cabin with score 0.11639631 and distance 0.88360369. BM25 returns no results. Because cabin is the only candidate, its first position provides no evidence of discrimination against unrelated records and does not contradict the Cowork result.

## Encoder identity

- Model: `sentence-transformers/static-retrieval-mrl-en-v1`
- Revision: `f60985c706f192d45d218078e49e5a8b6f15283a`
- Model SHA-256: `164fc63ee9f9267be7378fcbd7df99d09788a2f45244c92aa99ae5a574925716`
- Tokenizer SHA-256: `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`
- Preprocessing: `ese-bert-uncased-wordpiece-v1`
- Dimensions/storage: 512 / f32

## Freshness checks

The same test updates m1 to `mountain telescope observatory` and confirms it ranks first for that exact query, verifies the old BM25 `ramen` term disappears, deletes m1, and verifies semantic results no longer contain it. After a checkpoint and reopening the runtime, m1 remains absent and both diagnostic counts are two. Restoring the original m1 reproduces the original semantic results exactly. These checks passed.

The separate HTTP harness `python3 scripts/cloud/reproduce_search.py` also passed against a disposable localhost server and worker. Both queries returned the same rankings and scores as the runtime test. Additive semantic/BM25 builds retained all three source records, diagnostics reported three vectors, and update/delete plus full manager stop/restart and restoration passed. The harness uses a short `/tmp` root because the default macOS temporary directory made worker startup fail (the longer worker socket path is a likely cause, not separately diagnosed). No production service was contacted.
