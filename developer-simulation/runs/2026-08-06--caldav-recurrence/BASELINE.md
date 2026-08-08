# CalDAV recurrence baseline model

This is the baseline supplied by the scenario, written before selecting a
BogKit component or designing the prototype.

## Current data and write path

- SQLite is authoritative.
- An event row contains a UID, start and end, a TZID, a recurrence rule,
  exclusions, and occurrence overrides.
- The HTTP/CalDAV layer is already working and is outside this trial.
- The current expander walks daily, weekly, and monthly rules in local wall
  time, then converts each result to UTC.
- An edit replaces the whole event row.
- Output rows are emitted in insertion order.

## Baseline behavior versus the acceptance criteria

The baseline has no explicit occurrence identity, no canonical distinction
between a local date and a local date-time, no documented policy for a DST gap
or fold, no ordered publication key, and no materialization checkpoint. It
therefore cannot establish the required guarantees by inspection:

| Required guarantee | Baseline risk |
| --- | --- |
| Oracle agreement across DST gaps/folds | Local wall-time expansion is underspecified at nonexistent and repeated times. |
| All-day events keep their date | Treating an all-day date as local midnight before UTC conversion can move it to the prior or next UTC date. |
| Exclusions, cancellations, and overrides are stable | Whole-row replacement and UTC-only identity can lose the distinction between a deleted original occurrence and its replacement. |
| No duplicate `(UID, occurrence)` records | Insertion-order rows have no enforced unique occurrence key. |
| Replacement occurrences remain | An override needs the original recurrence identity plus its replacement payload; a row replacement alone does not provide that identity. |
| Reordered input and host-time-zone independence | Insertion order and implicit host conversion make output order and possibly values depend on input or process environment. |
| Single-event edit isolation | Replacing a whole row gives no dependency boundary for rebuilding only one UID. |
| Interruption recovery | There is no durable per-event/materialization state or atomic publish marker in the baseline. |
| No partial publication on invalid input | Validation and publication are not described as separate phases. |
| 128 MiB / five-second workload | The baseline gives no bounded-memory plan and has no measured implementation to benchmark. |

## Minimal corrected model for the prototype

The trial will represent an occurrence by `(uid, recurrence_id)` where the
recurrence ID is a local date for all-day events and a local wall-time value
for timed events. A replacement keeps that recurrence ID and changes only the
occurrence payload. A cancellation keeps the identity but emits no row.

Expansion will happen in the event's declared zone using only the supplied
transition table. Floating times will use the supplied fixed transition table
named `FLOATING` (and never the host zone); UTC values will bypass local
conversion. All-day values will remain civil dates and will not be converted to
UTC.

The output key will be `(uid, occurrence_kind, recurrence_id, start_utc_or_date)`
with a stable serialized sort order. Input validation will complete before the
temporary output is renamed into place. A per-UID state file will make a
single-event edit and an interrupted materialization resumable.

## Component fit decision, before implementation

Fold's `KeyedStream` is a good fit for one narrow boundary: keyed upsert and
remove semantics for event masters, with an atomic transaction and durable
retraction. Its `Table` sink also demonstrates the last-writer-wins behavior
needed by an occurrence key. Fold does not supply iCalendar recurrence rules,
civil-date arithmetic, DST gap/fold policy, supplied transition-table parsing,
SQLite authority, or atomic publication of a JSONL artifact. The trial will
therefore keep calendar semantics and publication in ordinary Rust and use a
small keyed Fold materialization only where that behavior is directly useful.

This is a prototype fit assessment, not a claim about the unavailable
production service. The supplied 5,000-case oracle and reference machine are
not present in this checkout, so those acceptance points can only be exercised
with the local deterministic fixture and measured as a bounded demonstration.
