# Container yard planner prototype

This is a standalone, advisory Rust command-line prototype for one frozen,
normalized yard block. It validates a snapshot, proposes pickups in the requested
order, separately replays every move, and emits either a complete
`moves.json` or a non-executable `review.json`. It never connects to cranes or a
terminal operating system.

This is a **conservative proposal generator**, not a complete feasibility
solver. Its bounded heuristic can return review for a wave that has a legal
sequence. The exact three-stack reviewer witness is preserved in the acceptance
tests and `evidence/FEASIBLE_FALSE_NEGATIVE.md`.

## What it does

- Moves only the top container of an unfrozen stack.
- Rejects moves of customs-held containers.
- Checks destination capacity, heavy-below-light order, reefer sockets,
  maintenance freezes, and hazardous-neighbor exclusions.
- Uses deterministic eight-pickup lookahead when choosing among legal
  destinations. The main penalty is burying a container requested soon.
- Includes the described nearest-legal-slot baseline for comparison.
- Builds the entire proposal on a private copy. A planning failure or planner
  timeout returns only a review artifact, never the accumulated move prefix.
- Invalidates a previous current artifact before input processing and publishes
  one new `moves.json` or `review.json` by atomic rename. Malformed input or a
  publication failure leaves no current artifact.
- Replays transitions through a separately coded path before marking a proposal
  executable. That path shares the model's static snapshot and hazardous-rule
  definitions, so it is not described as fully independent.

## BogKit fit decision

No BogKit crate is used. The public README and examples describe Fold as a
durable incremental-view engine, ESE as static text embeddings, and ANNy as
nearest-neighbor vector search. This workload is deterministic constraint
planning over a frozen snapshot; it does not need durable incremental views,
text embeddings, or semantic vector search. Pulling one of those crates into
the safety-critical planning loop would add machinery without addressing the
constraint search. See `evidence/DECISION_AUDIT.md` for the full audit.

## Exact reproduction

Run these commands from this directory. Keeping the target directory in
`/private/tmp` ensures build output is not added to this archive.

```console
CARGO_TARGET_DIR=/private/tmp/container-yard-planner-target cargo test --offline --locked --release -p container-yard-planner --all-targets
cargo fmt -p container-yard-planner -- --check
CARGO_TARGET_DIR=/private/tmp/container-yard-planner-target cargo clippy --offline --locked -p container-yard-planner --all-targets --all-features -- -D warnings
CARGO_TARGET_DIR=/private/tmp/container-yard-planner-target cargo run --offline --locked --release -p container-yard-planner -- demo
```

The demo should report three baseline relocations, two lookahead relocations,
33% improvement on that one micro-geometry, a successful replay, and
deterministic output.

For a real input pair:

```console
CARGO_TARGET_DIR=/private/tmp/container-yard-planner-target cargo run --offline --release -- plan /absolute/path/yard.json /absolute/path/pickups.json /private/tmp/yard-plan
CARGO_TARGET_DIR=/private/tmp/container-yard-planner-target cargo run --offline --release -- verify /absolute/path/yard.json /absolute/path/pickups.json /private/tmp/yard-plan/moves.json
```

The `verify` command validates status flags, declared counts, the replay flag,
step and pickup ranks, immediate reasons, the exact rule-check lists, and then
applies the separate transition replay. Do not run it on `review.json`, which
intentionally contains no moves.

## Input shape

Containers are listed bottom-to-top in each stack. Weight class 5 is heaviest
and class 1 is lightest. Neighbor relations must be reciprocal so hazardous
adjacency cannot be interpreted differently in opposite directions.

```json
{
  "max_height": 5,
  "hazardous_exclusions": {"A": ["B"]},
  "stacks": [
    {
      "id": "B01-R1",
      "x": 1,
      "y": 1,
      "reefer_socket": true,
      "frozen": false,
      "neighbors": ["B01-R2"],
      "containers": [
        {
          "id": "CONT-1",
          "weight_class": 5,
          "reefer": true,
          "hazardous_group": null,
          "customs_hold": false
        }
      ]
    },
    {
      "id": "B01-R2",
      "x": 1,
      "y": 2,
      "reefer_socket": false,
      "frozen": false,
      "neighbors": ["B01-R1"],
      "containers": []
    }
  ]
}
```

```json
{"pickups": ["CONT-1"]}
```

Optional booleans, `hazardous_group`, `neighbors`, and `containers` default to
false, null, or an empty list as appropriate. Stack IDs and container IDs must
be unique. Coordinates are used only for the baseline's Manhattan-distance
ranking and deterministic tie-breaking uses stack ID.

## Outputs

`moves.json` contains ordered moves, relocation and pickup totals, a transition-
replay verification flag, the immediate reason for each move,
and the rule checks supporting every destination. `review.json` identifies the
first blocked pickup and categorizes the preventing conditions. Both formats
have stable struct field order, sorted diagnostic lists, and a trailing newline
for byte-for-byte comparison.

The output directory is a reusable, planner-owned current-result location for
the names `moves.json`, `review.json`, and `.yard-plan.tmp`. At the start of each
`plan` invocation, those previous owned artifacts are removed. The new result is
fully written and synced under the hidden temporary name, then renamed to its
exclusive canonical name. On malformed input or publication failure, neither
canonical result remains. Other filenames in the directory are left untouched.

## Scope of the evidence

The supplied trial did not include the stated 30 production evaluation JSON
files or the C# baseline output. The tests repeat one suffix-renamed
micro-geometry 27 times as a determinism observation: its result is 3 baseline
relocations versus 2 proposed relocations. This is one geometry, not 27 diverse
feasible cases. A separate 48-by-6, 1,280-container fixture observed 42 versus
41 relocations. Three small synthetic infeasible cases, focused constraints,
and the exact feasible false-negative witness are also included. These tests
demonstrate prototype mechanics; they establish neither 27/27 feasible
completion nor a production 20% improvement. See `evidence/ACCEPTANCE.md` and
`evidence/FINDINGS.md` before operational evaluation.

The planner timer begins after file reading and JSON parsing; long validation,
serialization, and filesystem operations do not cooperatively enforce one hard
end-to-end deadline. The included dense fixture is fast, and tested planner
timeout checkpoints return review safely, but this is not a general 10-second
end-to-end guarantee.

The simulator's temporary child workspace marker and lockfile were removed
during archival. This package is a member of the nested daily workspace and
uses the lab's single `developer-simulation/Cargo.lock`.
