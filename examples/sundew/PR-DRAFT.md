# PR draft — do not file until Matthew says ready

Filing `matthewrball/bogkit:sundew` → `flowercomputers/bogkit` is the official Bogathon 3 submission.

Account: `matthewrball` via `gh auth`. No “Generated with …” tags. Category: **agent support** only.

Paste the block below as the PR body.

---

## Hackathon submission

fork this repo, build your project (ideally via `./scripts/new-project.sh [project-name]`), then open a pr against upstream to acknowledge your entry.

### category

pick **one**:

- [x] agent support
- [ ] performance
- [ ] novel interface / gaming

### team

- **project name:** Sundew
- **team / author name(s):** Matthew Ball, Dustin Dannenhauer
- **contact (optional):** matthew.robert.ball@gmail.com

### what you built

Alinery-shaped chunks (ticket / artifact / comment / wiki) in one KeyedStream. Fold maintains BM25, ESE+HNSW, a kind-count table, and a token-budgeted briefing. A human comment resticks the pack; remove deletes a stale decision from every index (ANNy has no tombstone). Search is the index; the briefing is the product. Files remain the source of truth — this is the query engine that filesystem never had.

Alinery’s filesystem is the database. Sundew is the query engine it doesn’t have yet.

### how to run

```bash
# from repo root
cargo run -p sundew -- --script
cargo run -p sundew
# then open http://localhost:3000
# If 3000 is taken: SUNDEW_PORT=3012 cargo run -p sundew
# Full briefing (or press f) for the pizza-room path. Replay runs the clap.
```

`SUNDEW_PORT` overrides the port if 3000 is taken.

### demo / notes

Replay button. Do not require an Alinery install. Honest: packer/RRF is query-time, same as `examples/search`; indexes and kind counts are incremental.

Click **Replay** once: seed → comment jumps to #1 → stale `session-lifecycle` wiki leaves BM25, HNSW, and the table. **Copy briefing** is the next-session seed.

### checklist

- [x] i forked bog-kit and built my project in this fork
- [x] my project is runnable from this pr (crate name and run command above)
- [x] i selected exactly one category
- [x] this pr is my official hackathon submission acknowledgment
