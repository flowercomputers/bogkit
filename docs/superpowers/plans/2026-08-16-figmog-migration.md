# figmog standalone migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans.

**Goal:** Execute docs/superpowers/specs/2026-08-16-figmog-standalone-repo.md — figmog at the root of the (already-created, empty) `sanctuarycomputer/figmog` repo, fold via pinned git dep, cuts + shakeout done, CI + draft-release workflow in place, tagged v0.1.0 draft.

**Precondition:** the multi-file serve milestone is complete and pushed on `worktree-figmog` (its final state is what migrates). Do not start while any implementer is active on this worktree.

**Mechanics note (sandbox):** this session's git operations are confined to this worktree. The migration therefore happens on an **orphan branch** here (`figmog-standalone`), whose tree IS the new repo's root layout, pushed to `https://github.com/sanctuarycomputer/figmog.git` as `main` (`git push <url> figmog-standalone:main`). Each task = commits on that branch + push. After the final task, switch this worktree back to `worktree-figmog`.

**Spec:** docs/superpowers/specs/2026-08-16-figmog-standalone-repo.md (binding; read fully before any task)

## Global Constraints
- The spec's §6 standing rules apply to all new-repo content (determinism, frozen sinks, append-only enums, no fold patches, gates, no new deps).
- Every task ends green IN THE NEW LAYOUT: `cargo test`, `cargo clippy --no-deps -- -D warnings`, `cargo fmt --check` run at the orphan branch root.
- Commit trailer everywhere: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- The clog-fork branch `worktree-figmog` is never modified by this plan.

---

### Task 1: import + git-dep switch (the repo exists after this)

- [ ] **Step 1 — pin determination:** `git remote add bogkit-upstream https://github.com/flowercomputers/bogkit && git fetch bogkit-upstream` (in-worktree git op). Diff our `fold/` + `anny/` against `bogkit-upstream/main` (`git diff worktree-figmog:fold bogkit-upstream/main:fold` etc.). If IDENTICAL: pin = upstream main's current rev. If upstream moved: find the newest upstream rev whose fold tree matches ours (`git log bogkit-upstream/main -- fold` + diff per rev); if none matches (upstream diverged incompatibly), STOP and report — the spec's fallback (pin the sanctuarycomputer/clog fork rev instead) needs a controller ruling with the divergence summarized.
- [ ] **Step 2 — orphan branch + layout:** `git switch --orphan figmog-standalone`; populate from `worktree-figmog`'s tree (use `git restore --source worktree-figmog -- examples/figmog docs` then move): crate files from `examples/figmog/*` to root (src/, tests/, README.md → kept for now, Cargo.toml rewritten standalone: `[package] name figmog version 0.1.0 edition 2024 license MIT` + the git dep `fold = { git = "https://github.com/flowercomputers/bogkit", rev = "<the pin>" }`, same crates.io deps/dev-deps, NO workspace section); `docs/history/` gets the old spec + all figmog plan docs verbatim; LICENSE = MIT (year 2026, copyright sanctuary computer); minimal `.gitignore` (target/, .figmog/). Nothing else yet (CI, SPEC.md, README rewrite are later tasks).
- [ ] **Step 3 — build against the git dep:** `cargo test` at root (network fetch of bogkit occurs here). All 15x tests must pass unchanged — this proves the pin is faithful. Then clippy/fmt gates.
- [ ] **Step 4 — commit + push:** single commit `import figmog from sanctuarycomputer/clog (branch worktree-figmog, PR #1) as standalone crate` (+ trailer); `git push https://github.com/sanctuarycomputer/figmog.git figmog-standalone:main`.

### Task 2: the cuts

- [ ] Remove `src/bench.rs`, `src/repl.rs` (+ their lib.rs mods, Cmd::Bench variant + dispatch, bench/repl tests in tests/cli.rs + tests/serve.rs if any, corpus references). Remove `Cmd::Watch` + `cmd_watch` (keep helpers serve/sessions still use — compiler tells). Remove human-mode output: every read command prints `serde_json::to_string_pretty` only; delete the printer/table helpers; remove the `--json` flag (JSON is the only mode) — errors become JSON on stderr unconditionally; update every test that passed `--json` (mechanical flag removal) and any asserting human output (delete those assertions, keep the JSON ones). Spec §4 lists this as approved — the test edits here are authorized wholesale but must be enumerated in the report.
- [ ] Post-cut dead-code sweep: build with `-D warnings` (dead_code surfaces), remove orphans; manual pass over remaining `pub(crate)` items for zero-caller leftovers.
- [ ] Gates; commit `remove bench/REPL/watch and human output mode (JSON-only CLI)`; push.

### Task 3: debt payment (spec §4's list, verbatim scope)

- [ ] Fix each: variable_edges dedup (BTreeSet or remove + honest comment); merge_registry dedup by actual local names; hoist namespace check above per-proxied-call rtx; cache::store surfaces serialize errors (Result); wrong-type args errors say expected/got; `--interval` overflow clamp; serve stdout writes tolerate closed pipe (write! + map_err → clean exit); visibility mismatches (pub mod serve etc.); obj_map borrow instead of clone; README gains Tier-3 budget line (interim — full README rewrite is T4). Add focused tests where a fix changes observable behavior (wrong-type message, interval clamp).
- [ ] Gates; commit `pay down deferred review debt`; push.

### Task 4: structure + docs

- [ ] Split cli.rs (~1.4k lines): `src/cli/mod.rs` (clap types + dispatch + run), `src/cli/pull.rs` (pull/do_pull/PullError/open_store_checked/current helpers), `src/cli/read.rs` (read command fns), `src/cli/call.rs` (tools/call/import-variables). Pure moves; suite green unchanged.
- [ ] `docs/SPEC.md`: consolidated CURRENT-state spec (architecture, data model, pipeline, sync, serve/MCP tools incl. multi-file, proxy + cache, variables story, bench REMOVED — no version archaeology; ~the §2-§14 content that still exists, rewritten present-tense). Old spec stays in docs/history/ untouched.
- [ ] README.md rewrite per spec §2 (standalone: what it is, install from GitHub Releases incl. macOS quarantine note + the fold-license gate sentence while unresolved, quick start, MCP setup incl. multi-file/URL-addressed usage, CLI reference, tool tables, limitations). New-repo CLAUDE.md per spec §6.
- [ ] Gates + `cargo doc --no-deps` warning-free; commit `standalone docs and module structure`; push.

### Task 5: CI + release + tag

- [ ] `.github/workflows/ci.yml`: on push/PR to main — ubuntu + macos runners: `cargo test`, `cargo clippy --no-deps -- -D warnings`, `cargo fmt --check`.
- [ ] `.github/workflows/release.yml` per spec §5: on tag `v*` — matrix {aarch64-apple-darwin on macos-14, x86_64-apple-darwin on macos-13, x86_64-unknown-linux-gnu on ubuntu-latest}; `cargo build --release`; strip; `tar czf figmog-${TAG}-${TARGET}.tar.gz -C target/<triple>/release figmog`; sha256 into SHA256SUMS; `gh release create "$TAG" --draft --title "$TAG"` + upload artifacts (use `softprops/action-gh-release` OR plain `gh` CLI — prefer plain `gh`, zero third-party actions beyond actions/checkout + dtolnay/rust-toolchain or rustup manual; document the choice).
- [ ] Push; verify CI runs green on the actual repo (`gh run watch/list -R sanctuarycomputer/figmog`); fix-forward if runner reality differs (allowed: iterative commits, each pushed, until green — list them).
- [ ] Tag `v0.1.0`, push tag, confirm the DRAFT release appears with 3 artifacts + checksums (`gh release view v0.1.0 -R sanctuarycomputer/figmog`). DO NOT publish the draft (fold-license gate; publishing is the user's manual act).
- [ ] Final: switch this worktree back to `worktree-figmog`. Commit nothing further there.

## Self-review checklist
- Spec §2 layout → T1/T4; §3 dep+pin+gate → T1 (+README sentence T4); §4 cuts+debt → T2/T3; §5 release → T5 (draft-only honored); §6 CLAUDE.md → T4; §7 sequencing → precondition + task order; §8 non-goals respected (no tap, no signing, no filter-repo).
- Risk center: T1's pin-parity check (fold drift would silently change engine behavior — the full-suite gate at T1 Step 3 is the proof).
