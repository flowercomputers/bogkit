# figmog serve (MCP server) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `figmog serve` — an MCP stdio server over the figmog store with integrated background sync, so agents query the mirror through MCP tools with always-fresh data.

**Architecture:** Spec §11 of `docs/superpowers/specs/2026-08-15-figmog-build-design.md` (read it first — it is the binding authority). Three moves: extract the CLI's read logic into a shared `query` module returning JSON; implement a dependency-free JSON-RPC/MCP protocol core; run one main loop that owns the store, answering requests between poll ticks.

**Tech Stack:** No new dependencies. JSON-RPC hand-rolled over `serde_json`; threads + `std::sync::mpsc` for the stdin reader; `std::process` in tests.

**Spec:** docs/superpowers/specs/2026-08-15-figmog-build-design.md (§11)

## Global Constraints

- Zero new crates in Cargo.toml. Stdout in serve mode carries ONLY newline-delimited JSON-RPC frames; all logging to stderr.
- All v1 behavior unchanged: every existing test keeps passing without modification (except moves of test-internal imports if a type relocates). The CLI's human/JSON output is byte-identical.
- Determinism rules hold (sorted outputs, no HashMap iteration at boundaries).
- Gates per task: `cargo test -p figmog`, `cargo clippy -p figmog --no-deps -- -D warnings`, `cargo fmt -p figmog --check` all clean. Zero diff to fold/ese/anny.
- If `cargo` is not on PATH, prefix commands with `export PATH="$HOME/.cargo/bin:$PATH" && `.
- Commits end with the `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` trailer.

---

### Task 1: `query.rs` — extract read logic from the CLI

**Files:**
- Create: `examples/figmog/src/query.rs`
- Modify: `examples/figmog/src/cli.rs`, `examples/figmog/src/lib.rs` (add `pub mod query;`)
- Test: existing `tests/cli.rs` must pass unchanged (that IS the test of this refactor)

**Interfaces:**
- Produces `pub fn`s in `figmog::query`, each taking the concrete reader types it needs (same signatures style as today's `cmd_*` — generic over `R: Readable`) and returning `Result<serde_json::Value, String>`:
  - `status(nodes, meta) -> Result<Value, String>` — the object today's `cmd_status` builds
  - `pages(nodes, by_type) -> Result<Value, String>` — JSON array
  - `tree(nodes, children, by_type, id: Option<String>, depth: Option<usize>) -> Result<Value, String>` — move `TreeNode`, `build_tree`, `tree_to_json` here; `TreeNode` and `build_tree` stay `pub` so the CLI's human printer can render from the same structure: also expose `pub fn tree_nodes(...) -> Result<TreeNode, String>` and have `tree()` wrap it
  - `node(nodes, children, id: String, with_children: bool) -> Result<Value, String>`
  - `find(nodes, by_type, node_type: String, page: Option<String>) -> Result<Value, String>`
  - `search(text, nodes, query: &str, limit: usize) -> Result<Value, String>`
  - `instances(nodes, components, component_sets, instances_of, target: &str) -> Result<Value, String>` (move `resolve_component_ids` here as a private fn)
  - `components(nodes, components, component_sets) -> Result<Value, String>`
  - `styles(nodes, styles, styled_by, style_type: Option<String>, values: bool) -> Result<Value, String>`
  - `uses(nodes, styled_by, bound_to, id: &str) -> Result<Value, String>`
  - `vars(nodes, variables, variable_collections, id_filter: Option<String>) -> Result<Value, String>`
- Consumed by: cli.rs `cmd_*` (become printers: call `query::*`, then either `println!("{}", serde_json::to_string(...))` in json mode or render human lines from the returned Value/TreeNode exactly as today), and by Task 3's tool handler.

- [ ] **Step 1:** Create `query.rs` by MOVING the body logic of each `cmd_*` read function (everything between reader access and printing) plus `TreeNode`/`build_tree`/`tree_to_json`/`resolve_component_ids` out of `cli.rs`. The JSON each function returns is exactly the Value the old code printed in `--json` mode. Human rendering stays in `cli.rs`, rebuilt from the returned Value (or `TreeNode` for tree). Node-id normalization (`normalize_node_id`) stays at the CLI/tool boundary — `query::*` receives already-normalized ids EXCEPT where today's code normalizes internally; preserve today's exact behavior.
- [ ] **Step 2:** Rewrite each `cmd_*` in `cli.rs` as a thin printer over `query::*`. Doc-comment `query.rs` (module: "One source of truth for every read answer — shared by the CLI printers and the MCP tools.").
- [ ] **Step 3:** Run the full suite: `cargo test -p figmog`. Every existing test must pass WITHOUT edits — if a cli test fails, the refactor changed behavior; fix the refactor, not the test. Then clippy + fmt gates.
- [ ] **Step 4:** Commit: `refactor(figmog): extract query layer shared by CLI and MCP`

---

### Task 2: `mcp.rs` — protocol core

**Files:**
- Create: `examples/figmog/src/mcp.rs`
- Modify: `examples/figmog/src/lib.rs` (add `pub mod mcp;`)
- Test: unit tests in `mcp.rs`

**Interfaces:**
- Produces:
  ```rust
  /// One registered tool: metadata for tools/list.
  pub struct ToolDef {
      pub name: &'static str,
      pub description: &'static str,
      /// JSON Schema for the tool's arguments.
      pub input_schema: serde_json::Value,
  }
  /// Executes a tools/call. Ok(v) => success content; Err(msg) => isError content.
  pub trait ToolHandler {
      fn call(&mut self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value, String>;
  }
  /// Handle one incoming JSON-RPC message. Returns the response frame to
  /// write, or None for notifications (and for malformed input handled
  /// via the returned parse-error frame — see below).
  pub fn handle_message(
      raw: &str,
      tools: &[ToolDef],
      handler: &mut dyn ToolHandler,
  ) -> Option<serde_json::Value>;
  pub const SERVER_NAME: &str = "figmog";
  ```
- Behavior contract (unit-test each):
  - Parse failure → `Some({jsonrpc:"2.0", id: null, error:{code:-32700, message:"parse error"}})`.
  - `initialize` → result `{protocolVersion: <echo the client's, or "2025-06-18" if absent>, capabilities: {tools: {}}, serverInfo: {name: "figmog", version: env!("CARGO_PKG_VERSION")}}`.
  - `notifications/initialized` (and any method starting `notifications/`) → `None`.
  - `ping` → result `{}`.
  - `tools/list` → `{tools: [{name, description, inputSchema}...]}` from the `ToolDef` slice, in slice order.
  - `tools/call` with `{name, arguments}` → invoke handler; Ok(v) → result `{content: [{type:"text", text: serde_json::to_string(&v)}], isError: false}`; Err(msg) → result `{content:[{type:"text", text: msg}], isError: true}`. Unknown tool name → handler returns Err (Task 3 handler) — but `mcp.rs` itself must also map a `name` missing from `tools` to the same isError shape without calling the handler.
  - Any other method with an `id` → error `-32601` "method not found". Requests without `id` (notifications) → `None`.
- [ ] **Step 1:** Write the failing unit tests for every bullet above (scripted `&str` → expected `Value` assertions; a `NullHandler` test double returning `Ok(json!({"ok":true}))` / `Err("boom")` by tool name).
- [ ] **Step 2:** Verify compile failure, implement, iterate to green. Gates.
- [ ] **Step 3:** Commit: `feat(figmog): MCP protocol core (JSON-RPC over stdio frames)`

---

### Task 3: `serve.rs` — the serve loop + CLI wiring

**Files:**
- Create: `examples/figmog/src/serve.rs`
- Modify: `examples/figmog/src/lib.rs` (add `pub mod serve;`), `examples/figmog/src/cli.rs` (add `Serve` variant + dispatch)

**Interfaces:**
- Produces: `pub fn run_serve(db: &crate::cli::Db, file: Option<String>, interval: u64, no_watch: bool) -> Result<(), String>` (make `Db` and the small helpers it needs `pub(crate)`; adjust visibility minimally). CLI: `figmog serve [file] [--interval N (default 10)] [--no-watch]`.
- Loop design (spec §11): spawn a thread reading `stdin` lines into an `mpsc::Sender<String>`; main loop owns the store (opened via `open_store!` at this concrete site) and a `Watcher` seeded from the stored watermark; `recv_timeout(until_next_tick)` — on message: `mcp::handle_message` → write response + `\n` to stdout, flush; on timeout (and `!no_watch`): tick → on `Changed` run the pull sequence inline (fetch via `UreqApi`, `flatten_file`, `collect_sweepable` in `rtx`, `store::sync`), honoring the existing `pull_failure_wait` backoff discipline and watcher-reset-on-failure rule; on `Wait{after}` extend the next deadline. Startup: if the store has no meta row and `no_watch` is false, do an initial pull before serving. eprintln! one startup line (name, file key, watch on/off).
- Tool registry: the 12 tools from spec §11's table, descriptions stating "reads the local mirror (no Figma API cost)" vs `figma_sync`'s "fetches from Figma (spends Tier-1 rate budget)". Handler: match tool name → normalize ids (`normalize_node_id` where the arg is a node id) → `st.rtx(|readers| query::*(…))` → the returned Value. `figma_sync` → the inline pull sequence → churn JSON. Unknown args types → Err(msg).
- [ ] **Step 1:** Implement `serve.rs` + wire the CLI variant. Keep every closure at the concrete `open_store!` site (the pipeline type is unnameable — same pattern as `dispatch`).
- [ ] **Step 2:** `cargo test -p figmog` (all green — no new tests yet), clippy, fmt. Manual smoke: `printf '…initialize…\n…tools/list…\n' | cargo run -p figmog -- serve --no-watch --db <fixture db>` shows two frames on stdout.
- [ ] **Step 3:** Commit: `feat(figmog): figmog serve — MCP stdio server with integrated sync`

---

### Task 4: end-to-end serve test + docs

**Files:**
- Create: `examples/figmog/tests/serve.rs`
- Modify: `examples/figmog/README.md`, workspace `README.md` (figmog bullet mentions MCP)

**Interfaces:** none new.

- [ ] **Step 1:** Write `tests/serve.rs`: build a fixture DB (reuse the `pull --from-file` pattern from `tests/cli.rs` — copy the `fixture_db()` helper or share via `tests/common`), then `std::process::Command` the compiled binary (`assert_cmd::cargo::cargo_bin("figmog")` gives the path) with `serve --no-watch --db <db>`, piped stdio. Write frames, read responses line-by-line with a read timeout guard (wrap reader thread + channel, or set a generous `wait_with_output` after closing stdin — closing stdin must terminate the loop: reader thread sees EOF, sender drops, `recv_timeout` returns Disconnected → clean exit; implement that exit path in Task 3 if missing). Assertions:
  - initialize response echoes id 1 and `serverInfo.name == "figmog"`
  - `tools/list` returns exactly 12 tools incl. `figma_search` and `figma_sync`
  - `tools/call figma_search {query:"garden"}` → `isError:false`, text parses to JSON whose first hit id is `1:2`
  - `tools/call figma_get_node {id:"1-2"}` → normalized, `name == "Title"`
  - `tools/call figma_get_node {id:"99:99"}` → `isError:true`
  - unknown method → error `-32601`; unknown tool → `isError:true`
- [ ] **Step 2:** README: new "Use from agents (MCP)" section — what `serve` is (server + built-in sync, one process), the `claude mcp add figmog -- <abs path to>/target/debug/figmog serve <file-url>` snippet (note: build first with `cargo build -p figmog`; or `--db`/`--no-watch` for offline), the 12-tool table (copy spec §11's), the note that only `figma_sync` spends rate budget. Workspace README bullet gains "and an MCP server (`figmog serve`)".
- [ ] **Step 3:** Full gates: `cargo test -p figmog` (now incl. serve e2e), clippy, fmt, `cargo test -p fold`, `cargo doc -p figmog --no-deps`.
- [ ] **Step 4:** Commit: `feat(figmog): serve e2e tests and MCP docs`

## Self-review checklist

- Spec §11 coverage: architecture → T3; query refactor → T1; protocol behaviors → T2 (all seven bullets are unit-tested); tools table → T3 registry + T4 README; testing section → T2 unit / T1 equivalence / T4 e2e. Non-goals respected (no resources/prompts, stdio only).
- The T1 refactor is the risk center: its acceptance gate ("existing tests pass unmodified") is what keeps v1 behavior frozen.
- T3's EOF-exit contract is stated in T4 Step 1 because the test depends on it; implementer of T3 must read T4's step (noted in dispatch).
