---
name: contextflywheel
description: Maintain evidence-backed, versioned beliefs while working on a long-running mission.
---

# ContextFlywheel

Use ContextFlywheel whenever an investigation forms or changes a factual hypothesis.

1. Read the injected `CONTEXTFLYWHEEL MISSION STATE` before acting.
2. Treat `CURRENT BELIEFS` as current and `HISTORICAL — NOT CURRENT` only as history.
3. Record important tool evidence with:
   `contextflywheel context_record_evidence --kind tool-result --text "..."`
4. Copy the returned record ID into a belief revision:
   `contextflywheel context_revise_belief --key KEY --statement "..." --confidence 0.0..1 --evidence ID [--expected-version N]`
5. Record an approach that failed with:
   `contextflywheel context_record_failure --text "..."`
6. Inspect current memory with `contextflywheel context_get --query "..."` and prior state with `contextflywheel context_history --step N`.

Never store credentials or secrets. Never promote a superseded belief without new evidence.
