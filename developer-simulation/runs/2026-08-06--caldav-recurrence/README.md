# CalDAV recurrence prototype

This standalone CLI evaluates a deliberately small, deterministic calendar
occurrence model. It is a prototype for the August 6 developer-simulation
trial, not a CalDAV server or a replacement for an existing SQLite service.

## Supported input

Events are JSONL records with `uid`, `kind` (`timed` or `all_day`), `start`,
`end`, optional `tzid`, one `DAILY`, `WEEKLY`, or `MONTHLY` `rrule`, `exdate`,
and confirmed or cancelled occurrence overrides. Timed values use
`YYYY-MM-DDTHH:MM:SS`; UTC values may use RFC3339 offsets. All-day values are
civil dates. Transition JSON supplies the complete fixed offset history for
each named zone and a `FLOATING` zone.

The model chooses the earlier UTC instant for a fall-back fold and shifts a
nonexistent spring-forward wall time forward by the gap. It rejects unsupported
rules, malformed values, duplicate canonical occurrence identities, overrides
that do not belong to the generated recurrence set, and invalid expansions.
All-day query intersection uses half-open civil dates: an endpoint exactly at
midnight excludes that date, while a partial-day endpoint includes the date it
touches.

## Reproduce

From this directory, with the checked-in lockfile and cached dependencies:

```text
cargo test --offline --locked --all-targets
cargo fmt -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
cargo run --offline --bin caldav-recurrence-prototype -- \
  --events fixtures/smoke-events.jsonl \
  --transitions fixtures/smoke-zones.json \
  --from 2026-03-07T00:00:00Z --to 2026-03-12T00:00:00Z \
  --output /private/tmp/caldav-smoke-output.jsonl \
  --state-dir /private/tmp/caldav-smoke-state
```

The full reviewed workload is generated and run from the trial checkout with
the archived generator, then compared under `TZ=UTC` and
`TZ=Pacific/Honolulu`. The exact commands and observed results are in
`evidence/REPORT.md`.

Fold is used only for the event-master keyed upsert/remove boundary. Recurrence
semantics, civil-time conversion, SQLite authority, shard integrity, and
filesystem publication remain outside BogKit. The controlled interruption hook
tests preservation of the previous published output; it is not a claim about
process-crash, filesystem, power-loss, or directory-sync durability.
